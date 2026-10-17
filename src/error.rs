use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
            Self::AuthFailed | Self::ConnectFailed | Self::Tls => 9,
            Self::ServerError | Self::RequestTimeout => 10,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn serializes_as_snake_case_with_optional_fields() {
        let error = Error::new(
            Code::NotConnected,
            "the connection to irc.weblounge.org dropped",
        )
        .with_hint("run `louro status`");
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({
                "code": "not_connected",
                "message": "the connection to irc.weblounge.org dropped",
                "hint": "run `louro status`"
            })
        );

        let bare = Error::new(Code::NotAllowed, "#lounge is not in the channel allowlist");
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            json!({"code": "not_allowed", "message": "#lounge is not in the channel allowlist"})
        );
    }

    #[test]
    fn round_trips_through_json() {
        let error = Error::new(Code::NoSession, "could not reach louro")
            .with_hint("ask your operator to start it");
        let json = serde_json::to_string(&error).unwrap();
        assert_eq!(serde_json::from_str::<Error>(&json).unwrap(), error);
    }

    #[test]
    fn exit_codes_follow_the_design_table() {
        let contract = [
            (Code::VersionMismatch, 1),
            (Code::TooLong, 2),
            (Code::SessionExists, 3),
            (Code::NotConnected, 4),
            (Code::BlockedSecret, 5),
            (Code::Tls, 9),
            (Code::RequestTimeout, 10),
        ];
        for (code, exit) in contract {
            assert_eq!(code.exit_code(), exit, "{code:?}");
        }
    }
}
