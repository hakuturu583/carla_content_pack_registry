use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    /// Error response from the registry (`{"error": {"code", "message"}}`).
    #[error("{message}{}", hint(code))]
    Api { status: u16, code: String, message: String },
    #[error("not logged in to {0}; run `carla-pack login`")]
    NotLoggedIn(String),
    #[error("{0}")]
    Usage(String),
    #[error("interrupted")]
    Interrupted,
    /// The downloaded file is not a valid content pack (layout, manifest or checksums).
    #[error("{0}")]
    Pack(String),
    /// A valid pack built for another CARLA base release.
    #[error("{0}")]
    Incompatible(String),
    #[error("network error: {}", chain(.0))]
    Http(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unexpected response: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// Stable machine-readable code (the registry's error code where there is one).
    pub fn code(&self) -> &str {
        match self {
            Self::Api { code, .. } => code,
            Self::NotLoggedIn(_) => "not_logged_in",
            Self::Interrupted => "interrupted",
            Self::Pack(_) => "invalid_pack",
            Self::Incompatible(_) => "incompatible_pack",
            _ => "client_error",
        }
    }
}

/// What the user can do about a registry error code.
fn hint(code: &str) -> &'static str {
    match code {
        "unauthenticated" => "; run `carla-pack login`",
        "not_verified" => "; the owner must run `carla-pack bucket verify`",
        "membership_unverifiable" => "; check that your GitHub organization has approved the registry's OAuth App",
        _ => "",
    }
}

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
