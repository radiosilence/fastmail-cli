use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Authentication required. Run `fastmail-cli auth <token>` first.")]
    NotAuthenticated,

    /// Authentication was rejected by the server.
    ///
    /// The inner value is a `&'static str` by design — using a static literal
    /// ensures no call site can accidentally pass the token itself or another
    /// secret into this variant where it would then surface in error output.
    #[error("Invalid API token: {0}")]
    InvalidToken(&'static str),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JMAP error: {method} failed - {error_type}: {description}")]
    Jmap {
        method: String,
        error_type: String,
        description: String,
    },

    #[error("Mailbox not found: {0}")]
    MailboxNotFound(String),

    #[error("Email not found: {0}")]
    EmailNotFound(String),

    #[error("Identity not found for sending")]
    IdentityNotFound,

    #[error(
        "No identity found matching '{0}'. Run `fastmail-cli list identities` to see available identities."
    )]
    IdentityNotFoundForEmail(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Rate limited. Try again later.")]
    RateLimited,

    #[error("Server error: {0}")]
    Server(String),
}

pub type Result<T> = std::result::Result<T, Error>;
