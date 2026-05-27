use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("http transport error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json decode error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("nanokvm api returned code {code}: {msg}")]
    Api { code: i64, msg: String },

    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("websocket error: {0}")]
    Ws(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("screenshot timeout")]
    ScreenshotTimeout,
}

pub type Result<T> = std::result::Result<T, Error>;
