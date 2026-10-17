use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{Code, Error, Result};

pub const VERSION: u8 = 1;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Request {
    pub v: u8,
    #[serde(flatten)]
    pub op: Op,
}

impl Request {
    #[must_use]
    pub const fn new(op: Op) -> Self {
        Self { v: VERSION, op }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "op", content = "args", rename_all = "snake_case")]
pub enum Op {
    Status {},
    Wait(WaitArgs),
    Send(SendArgs),
    Quit {},
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WaitArgs {
    pub timeout_secs: Option<u64>,
    #[serde(default = "default_max")]
    pub max: usize,
}

const fn default_max() -> usize {
    20
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SendArgs {
    pub target: String,
    pub text: String,
    #[serde(default)]
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Message,
    Notice,
    Action,
}

/// What the client writes after printing a wait result; only then does the daemon move its cursor.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Ack {
    pub ack: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Response {
    pub v: u8,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

impl Response {
    /// # Errors
    ///
    /// Returns the daemon's error, or `version_mismatch` when the daemon speaks another version.
    pub fn into_result(self) -> Result<Value> {
        if self.v != VERSION {
            return Err(version_mismatch());
        }
        match (self.ok, self.error) {
            (true, _) => Ok(self.result.unwrap_or_default()),
            (false, Some(error)) => Err(error),
            (false, None) => Err(Error::new(
                Code::Internal,
                "the daemon failed without saying why",
            )),
        }
    }
}

impl From<Result<Value>> for Response {
    fn from(result: Result<Value>) -> Self {
        let (result, error) = match result {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(error)),
        };
        Self {
            v: VERSION,
            ok: error.is_none(),
            result,
            error,
        }
    }
}

#[must_use]
pub fn version_mismatch() -> Error {
    Error::new(
        Code::VersionMismatch,
        "the louro client and daemon speak different protocol versions",
    )
    .with_hint("restart the daemon with `louro quit` then `louro connect`")
}

/// Reads one JSON line, or `None` once the peer has closed the connection.
///
/// # Errors
///
/// Fails on I/O errors and on a line that isn't the expected JSON.
pub async fn read_line<T: DeserializeOwned>(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> io::Result<Option<T>> {
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&line)?))
}

/// # Errors
///
/// Fails on I/O errors.
pub async fn write_line(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &impl Serialize,
) -> io::Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::io::BufReader;
    use tokio::net::UnixStream;

    use super::*;

    fn parse(request: Value) -> Request {
        serde_json::from_value(request).unwrap()
    }

    #[test]
    fn parses_each_op() {
        assert_eq!(
            parse(json!({"v": 1, "op": "status", "args": {}})).op,
            Op::Status {}
        );
        assert_eq!(
            parse(json!({"v": 1, "op": "quit", "args": {}})).op,
            Op::Quit {}
        );
        assert_eq!(
            parse(json!({"v": 1, "op": "wait", "args": {"timeout_secs": 60, "max": 5}})).op,
            Op::Wait(WaitArgs {
                timeout_secs: Some(60),
                max: 5
            })
        );
        assert_eq!(
            parse(json!({"v": 1, "op": "send", "args": {
                "target": "#bots", "text": "waves", "kind": "action"
            }}))
            .op,
            Op::Send(SendArgs {
                target: "#bots".into(),
                text: "waves".into(),
                kind: Kind::Action
            })
        );
    }

    #[test]
    fn wait_and_send_args_have_defaults() {
        assert_eq!(
            parse(json!({"v": 1, "op": "wait", "args": {}})).op,
            Op::Wait(WaitArgs {
                timeout_secs: None,
                max: 20
            })
        );
        let Op::Send(send) =
            parse(json!({"v": 1, "op": "send", "args": {"target": "maria", "text": "thanks!"}})).op
        else {
            panic!("expected a send");
        };
        assert_eq!(send.kind, Kind::Message);
    }

    #[test]
    fn serializes_requests_in_the_wire_shape() {
        let request = Request::new(Op::Wait(WaitArgs {
            timeout_secs: Some(0),
            max: 20,
        }));
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"v": 1, "op": "wait", "args": {"timeout_secs": 0, "max": 20}})
        );
        assert_eq!(
            serde_json::to_value(Request::new(Op::Quit {})).unwrap(),
            json!({"v": 1, "op": "quit", "args": {}})
        );
    }

    #[test]
    fn responses_carry_either_a_result_or_an_error() {
        let ok = Response::from(Ok(json!({"target": "#bots", "lines": 1})));
        assert_eq!(
            serde_json::to_value(&ok).unwrap(),
            json!({"v": 1, "ok": true, "result": {"target": "#bots", "lines": 1}})
        );

        let not_allowed = Error::new(Code::NotAllowed, "#lounge is not an allowed channel");
        let failed = Response::from(Err(not_allowed.clone()));
        assert_eq!(
            serde_json::to_value(&failed).unwrap(),
            json!({"v": 1, "ok": false, "error": {
                "code": "not_allowed", "message": "#lounge is not an allowed channel"
            }})
        );
        assert_eq!(failed.into_result(), Err(not_allowed));
    }

    #[test]
    fn into_result_returns_the_result_or_a_fallback_error() {
        let sent = json!({"target": "#bots", "msgid": "c2m8w1u0pq7rs", "lines": 1});
        assert_eq!(Response::from(Ok(sent.clone())).into_result(), Ok(sent));

        let silent: Response = serde_json::from_value(json!({"v": 1, "ok": false})).unwrap();
        assert_eq!(silent.into_result().unwrap_err().code, Code::Internal);
    }

    #[test]
    fn a_response_from_another_version_is_a_mismatch() {
        let response: Response =
            serde_json::from_value(json!({"v": 2, "ok": true, "result": {}})).unwrap();
        assert_eq!(
            response.into_result().unwrap_err().code,
            Code::VersionMismatch
        );
    }

    #[tokio::test]
    async fn lines_round_trip_over_a_unix_stream() {
        let (client, server) = UnixStream::pair().unwrap();
        let mut client = BufReader::new(client);
        let mut server = BufReader::new(server);

        let request = Request::new(Op::Status {});
        write_line(&mut client, &request).await.unwrap();
        assert_eq!(read_line(&mut server).await.unwrap(), Some(request));

        write_line(&mut server, &Ack { ack: true }).await.unwrap();
        assert_eq!(
            read_line(&mut client).await.unwrap(),
            Some(Ack { ack: true })
        );

        drop(server);
        assert_eq!(read_line::<Ack>(&mut client).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_line_that_is_not_json_is_an_error() {
        let (client, server) = UnixStream::pair().unwrap();
        let mut client = BufReader::new(client);
        client.write_all(b"PRIVMSG #bots :hi\n").await.unwrap();
        let mut server = BufReader::new(server);
        assert!(read_line::<Request>(&mut server).await.is_err());
    }
}
