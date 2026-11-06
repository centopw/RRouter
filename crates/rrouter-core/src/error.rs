use serde_json::Value;

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
            _ => None,
        }
    }

    /// `Retry-After` value in seconds, when the error knows how long the client must wait.
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::Limited { retry_after_secs, .. } => Some(*retry_after_secs),
            _ => None,
        }
    }

    /// Errors that mean "this account cannot serve this request" — try the next
    /// candidate. `Unauthorized` is included because it is what the refresher
    /// returns for a dead refresh token or a connection with no credential: one
    /// broken account must never fail a request that a healthy account can serve.
    pub fn is_retryable_upstream(&self) -> bool {
        match self {
            Self::Upstream { status, .. } => {
                matches!(status, 401 | 403 | 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504 | 529)
            }
            Self::Transport(_) | Self::Timeout(_) | Self::Unauthorized(_) => true,
            _ => false,
        }
    }

    pub fn is_rate_limited(&self) -> bool {
        matches!(self, Self::Upstream { status: 429, .. })
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
}
