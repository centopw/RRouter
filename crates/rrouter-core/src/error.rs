use serde_json::Value;

/// How an upstream refused the *account*, as opposed to the request.
///
/// A refusal is a fact about the credential: re-sending the same request to the same
/// account cannot change it, and the two classes need opposite handling, which is why
/// they are a type rather than a string an operator has to interpret.
///
/// * [`Refusal::Terminated`] — **permanent**. The provider terminated the account; a
///   terms-of-service violation is the case seen in the wild. Retrying it forever hides a
///   dead account in rotation and spends a fallback slot on every request, so the
///   connection is retired (`is_active = 0` plus `connections.refusal`) and the dispatcher
///   fails over to the next account.
/// * [`Refusal::Verification`] — **recoverable**. A `VALIDATION_REQUIRED` answer arrives
///   on healthy, fully-quota'd accounts and clears once the operator completes the
///   provider's verification. It must never retire an account: the connection cools down
///   through the breaker and is eligible again when the cooldown expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Permanent account termination: the account leaves rotation for good.
    Terminated,
    /// Recoverable verification-required: cool the account down, keep it eligible.
    Verification,
}

impl Refusal {
    /// Stable token. It is both the client-visible error `code` and the value stored in
    /// `connections.refusal`, so one vocabulary names the same fact everywhere.
    pub fn code(self) -> &'static str {
        match self {
            Self::Terminated => "account_suspended",
            Self::Verification => "account_verification_required",
        }
    }

    /// Permanently refused accounts leave rotation; recoverable ones only cool down.
    pub fn is_permanent(self) -> bool {
        matches!(self, Self::Terminated)
    }
}

/// Crate-wide error. Every variant maps to an inbound HTTP status by the gateway.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("not found: {0}")]
    NotFound(String),

    /// Governance refused the request before any upstream was contacted. `code` is the
    /// stable machine-readable reason (`api_key_inactive`, `api_key_expired`,
    /// `model_not_allowed`) that clients branch on; the message is for humans.
    #[error("{message}")]
    Forbidden { code: &'static str, message: String },

    /// A per-key quota was already exhausted. `retry_after_secs` becomes the
    /// `Retry-After` header; for a lifetime budget it is an operator-action hint, not
    /// a promise that waiting helps.
    #[error("{message}")]
    Limited {
        code: &'static str,
        message: String,
        retry_after_secs: u64,
    },

    #[error("no eligible upstream for model {model}: {detail}")]
    NoRoute { model: String, detail: String },

    #[error("upstream {provider} returned {status}: {body}")]
    Upstream {
        provider: String,
        status: u16,
        body: String,
        /// Parsed upstream error payload when it was JSON; pass-through for clients.
        payload: Option<Value>,
    },

    /// The upstream refused the *account*, in one of the two ways [`Refusal`]
    /// distinguishes. A distinct variant rather than an [`Error::Upstream`], because the
    /// dispatcher must fail over without retrying this account on the same request, and a
    /// permanent refusal must retire it instead of leaving it in rotation. `message` is
    /// the client-visible sentence: it names the refusal, so a caller can tell a suspended
    /// account from a malformed request.
    #[error("{message}")]
    AccountRefused { refusal: Refusal, message: String },

    #[error("translation failed: {0}")]
    Translation(String),

    #[error("storage failure: {0}")]
    Store(String),

    #[error("transport failure: {0}")]
    Transport(String),

    #[error("timed out: {0}")]
    Timeout(String),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    /// HTTP status the gateway should return to the client for this error.
    pub fn status(&self) -> u16 {
        match self {
            Self::BadRequest(_) | Self::Translation(_) => 400,
            Self::Unauthorized(_) => 401,
            Self::Forbidden { .. } => 403,
            Self::NotFound(_) | Self::NoRoute { .. } => 404,
            Self::Limited { .. } => 429,
            // A terminated account is gone for good, so the answer must not invite a
            // retry: 403 says "not permitted", where 502 would suggest the gateway broke.
            Self::AccountRefused {
                refusal: Refusal::Terminated,
                ..
            } => 403,
            // Verification clears once a human acts, so it is a temporary
            // unavailability rather than a refusal of the caller's key.
            Self::AccountRefused {
                refusal: Refusal::Verification,
                ..
            } => 503,
            Self::Upstream { status, .. } => *status,
            Self::Timeout(_) => 504,
            Self::Store(_) | Self::Transport(_) | Self::Json(_) | Self::Io(_) => 502,
        }
    }

    /// Explicit error `code` for the response body, when the variant carries one.
    /// `None` means the status-derived default applies.
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::Forbidden { code, .. } | Self::Limited { code, .. } => Some(code),
            Self::AccountRefused { refusal, .. } => Some(refusal.code()),
            _ => None,
        }
    }

    /// `Retry-After` value in seconds, when the error knows how long the client must wait.
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::Limited {
                retry_after_secs, ..
            } => Some(*retry_after_secs),
            _ => None,
        }
    }

    /// Errors that mean "this account cannot serve this request" — try the next
    /// candidate. `Unauthorized` is included because it is what the refresher
    /// returns for a dead refresh token or a connection with no credential: one
    /// broken account must never fail a request that a healthy account can serve.
    /// `AccountRefused` is included for the same reason in both classes: another account
    /// may still serve, and the refused one is benched (permanently or for a cooldown)
    /// either way.
    pub fn is_retryable_upstream(&self) -> bool {
        match self {
            Self::Upstream { status, .. } => {
                matches!(
                    status,
                    401 | 403 | 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504 | 529
                )
            }
            Self::Transport(_) | Self::Timeout(_) | Self::Unauthorized(_) => true,
            Self::AccountRefused { .. } => true,
            _ => false,
        }
    }

    pub fn is_rate_limited(&self) -> bool {
        matches!(self, Self::Upstream { status: 429, .. })
    }

    /// The status the *provider* answered with, for attempt rows and breaker bookkeeping.
    ///
    /// Not [`Error::status`]: a verification-required account answers `403` upstream but
    /// reaches the client as `503`, because the gateway is translating "temporarily
    /// unavailable" rather than repeating a verdict about somebody's account. Bookkeeping
    /// keeps the provider's number, which is what the breaker's auth-failure window keys
    /// on, so such an account is cooled down for that window instead of being probed again
    /// on the next request.
    pub fn upstream_status(&self) -> u16 {
        match self {
            // Every account-level refusal in the taxonomy is a 403 at the provider:
            // Cloud Code Assist answers both `TOS_VIOLATION` and `VALIDATION_REQUIRED`
            // with that status, and the class is carried by `refusal`, not by the number.
            Self::AccountRefused { .. } => 403,
            _ => self.status(),
        }
    }

    /// The refusal class, when this error is an account-level upstream refusal.
    ///
    /// Callers branch on it rather than on the HTTP status: `403` also means "bad API
    /// key on this request" and `503` also means "provider is down", and only this
    /// variant says the answer was about the account.
    pub fn refusal(&self) -> Option<Refusal> {
        match self {
            Self::AccountRefused { refusal, .. } => Some(*refusal),
            _ => None,
        }
    }

    /// Build an account-level refusal from `detail`, the upstream's own wording.
    ///
    /// The sentence names the refusal plainly, because neither audience can act on a
    /// generic "translation failed": the client would retry a request that can never
    /// succeed, and the operator reading the usage row could not tell which account to
    /// re-authenticate or appeal.
    pub fn account_refused(provider: &str, refusal: Refusal, detail: &str) -> Self {
        let head = match refusal {
            Refusal::Terminated => format!(
                "upstream account suspended: the {provider} account was disabled by the \
                 provider and has been removed from rotation"
            ),
            Refusal::Verification => format!(
                "upstream account needs verification: the {provider} account must complete \
                 the provider's verification before it can serve requests"
            ),
        };
        let detail = detail.trim();
        let message = if detail.is_empty() {
            head
        } else {
            format!("{head} ({detail})")
        };
        Self::AccountRefused { refusal, message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn governance_errors_map_to_403_and_429_with_their_code() {
        let forbidden = Error::Forbidden {
            code: "model_not_allowed",
            message: "model not allowed".into(),
        };
        assert_eq!(forbidden.status(), 403);
        assert_eq!(forbidden.code(), Some("model_not_allowed"));
        assert_eq!(forbidden.retry_after_secs(), None);

        let limited = Error::Limited {
            code: "rate_limited",
            message: "slow down".into(),
            retry_after_secs: 1,
        };
        assert_eq!(limited.status(), 429);
        assert_eq!(limited.code(), Some("rate_limited"));
        assert_eq!(limited.retry_after_secs(), Some(1));

        // A governance refusal is our decision, never a retry signal for the dispatcher.
        assert!(!forbidden.is_retryable_upstream());
        assert!(!limited.is_retryable_upstream());
    }

    /// A terminated account and a verification-required account differ in every
    /// observable way: the status the client sees, and whether the account is retired.
    #[test]
    fn a_terminated_account_is_terminal_and_a_verification_is_recoverable() {
        let banned = Error::account_refused(
            "google-antigravity",
            Refusal::Terminated,
            "The service has been disabled in this account for violation of our Terms of Service.",
        );
        assert_eq!(
            banned.status(),
            403,
            "a dead account is not a gateway fault"
        );
        assert_eq!(banned.code(), Some("account_suspended"));
        assert_eq!(banned.refusal(), Some(Refusal::Terminated));
        assert!(banned.refusal().is_some_and(Refusal::is_permanent));
        // Fail over to the next account: a permanent refusal of *this* one says nothing
        // about the request.
        assert!(banned.is_retryable_upstream());
        // The message names the refusal instead of flattening to a translation error, and
        // keeps the provider's own wording for the operator.
        let text = banned.to_string();
        assert!(text.contains("account suspended"), "{text}");
        assert!(
            text.to_lowercase().contains("terms of service"),
            "the provider's own wording rides along: {text}"
        );

        let verify = Error::account_refused(
            "google-antigravity",
            Refusal::Verification,
            "Verify your account to continue",
        );
        assert_eq!(
            verify.status(),
            503,
            "temporarily unavailable, not forbidden"
        );
        assert_eq!(verify.code(), Some("account_verification_required"));
        assert_eq!(
            verify.upstream_status(),
            403,
            "bookkeeping keeps the provider's own number: that is the breaker's cooldown key"
        );
        assert_eq!(verify.refusal(), Some(Refusal::Verification));
        assert!(
            !verify.refusal().is_some_and(Refusal::is_permanent),
            "verification must never retire an account"
        );
        assert!(verify.is_retryable_upstream());
        assert!(verify.to_string().contains("needs verification"));
        assert_eq!(Refusal::Terminated.code(), "account_suspended");
        assert_eq!(
            Refusal::Verification.code(),
            "account_verification_required"
        );
    }

    /// With no upstream wording the message still reads as a sentence, and an ordinary
    /// error is not mistaken for a refusal.
    #[test]
    fn refusal_messages_survive_an_empty_detail() {
        let bare = Error::account_refused("p", Refusal::Terminated, "  ");
        let text = bare.to_string();
        assert!(text.ends_with("removed from rotation"), "{text}");
        assert!(!text.contains("()"), "{text}");

        let plain = Error::Upstream {
            provider: "p".into(),
            status: 403,
            body: "forbidden".into(),
            payload: None,
        };
        assert_eq!(plain.refusal(), None, "an ordinary 403 is not a refusal");
    }
}
