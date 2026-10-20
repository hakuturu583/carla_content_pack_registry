use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// Error response from the registry (`{"error": {"code", "message"}}`).
    #[error("{message} [{code}, HTTP {status}]")]
    Api { status: u16, code: String, message: String },
    #[error("not logged in to {0}; run `carla-pack login`")]
    NotLoggedIn(String),
    #[error("{0}")]
    Usage(String),
    #[error("network error: {}", chain(.0))]
    Http(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unexpected response: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// "error sending request: connection refused" instead of only the outermost message.
fn chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(s) = cur {
        out.push_str(": ");
        out.push_str(&s.to_string());
        cur = s.source();
    }
    out
}
