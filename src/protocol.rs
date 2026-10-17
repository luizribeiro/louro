use std::num::NonZeroUsize;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

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
    pub max: NonZeroUsize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SendArgs {
    pub target: String,
    pub text: String,
    pub kind: Kind,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Message,
    Notice,
    Action,
}

/// What the client writes after printing a wait result; only then does the daemon move its cursor.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Ack {
    pub ack: bool,
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
    use serde_json::{Value, json};
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
                max: NonZeroUsize::new(5).unwrap()
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
    fn serializes_requests_in_the_wire_shape() {
        let request = Request::new(Op::Wait(WaitArgs {
            timeout_secs: Some(0),
            max: NonZeroUsize::new(20).unwrap(),
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
