use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Authentication required. Run `fastmail-cli auth <token>` first.")]
    NotAuthenticated,

    #[error("Invalid API token: {0}")]
    InvalidToken(String),

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

    #[error("Config error: {0}")]
    Config(String),

    #[error("Rate limited. Try again later.")]
    RateLimited,

    #[error("Server error: {0}")]
    Server(String),
}

pub type Result<T> = std::result::Result<T, Error>;
