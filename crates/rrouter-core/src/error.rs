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
            Self::NotFound(_) | Self::NoRoute { .. } => 404,
            Self::Upstream { status, .. } => *status,
            Self::Timeout(_) => 504,
            Self::Store(_) | Self::Transport(_) | Self::Json(_) | Self::Io(_) => 502,
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
