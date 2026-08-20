use std::{fmt::Display, io};

#[derive(Debug)]
pub enum CoreError {
    Message(String),
    Io(io::Error),
    Json(serde_json::Error),
    Sql(rusqlite::Error),
    Http(reqwest::Error),
}

impl Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(message) => write!(f, "{message}"),
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "{error}"),
            Self::Sql(error) => write!(f, "{error}"),
            Self::Http(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for CoreError {}

impl From<io::Error> for CoreError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<rusqlite::Error> for CoreError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sql(value)
    }
}

impl From<reqwest::Error> for CoreError {
    fn from(value: reqwest::Error) -> Self {
        Self::Http(value)
    }
}
