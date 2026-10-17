use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    Internal,
    VersionMismatch,
    Usage,
    TooLong,
    NoSession,
    SessionExists,
    NotConnected,
    NotAllowed,
    BlockedSecret,
    Unsupported,
    AuthFailed,
    ConnectFailed,
    Tls,
    ServerError,
    RequestTimeout,
}

impl Code {
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Internal | Self::VersionMismatch => 1,
            Self::Usage | Self::TooLong => 2,
            Self::NoSession | Self::SessionExists => 3,
            Self::NotConnected => 4,
            Self::NotAllowed | Self::BlockedSecret => 5,
            Self::Unsupported => 7,
            Self::AuthFailed | Self::ConnectFailed | Self::Tls => 9,
            Self::ServerError | Self::RequestTimeout => 10,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Error {
    pub code: Code,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, " ({hint})")?;
        }
        Ok(())
    }
}
