use std::env;
use std::fmt::Display;
use std::num::NonZeroUsize;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::error::{Code, Error, Result};
use crate::irc::{HistoryQuery, Outgoing, Typing};

pub const VERSION: u8 = 6;

#[derive(Deserialize, Serialize)]
pub struct Request {
    pub v: u8,
    #[serde(flatten)]
    pub op: Op,
}

/// A request to the daemon; the commands that only forward one parse straight into it.
#[derive(Deserialize, Serialize, clap::Subcommand)]
#[serde(tag = "op", content = "args", rename_all = "snake_case")]
pub enum Op {
    /// Connection state, nick, channels, and what the bot may do (grants)
    Status {},
    /// Block until matching events arrive
    ///
    /// Event ids and the cursor restart when the daemon restarts, so neither is stable across
    /// runs.
    Wait {
        /// Seconds to block for, or `none` to wait forever; `0` polls
        #[arg(long, default_value = "60", value_parser = parse_timeout, value_name = "SECS|none")]
        timeout: u64,
        /// Maximum number of events to return
        #[arg(long, default_value = "20", value_name = "N")]
        max: NonZeroUsize,
    },
    #[command(skip)]
    Send(Outgoing),
    /// Members of a channel the bot is in
    Names {
        /// A channel the bot is in
        channel: String,
    },
    /// A channel's topic, or set it
    Topic {
        /// A channel the bot is in
        channel: String,
        /// Set the topic to this text instead
        #[arg(long, value_name = "TEXT")]
        set: Option<String>,
    },
    /// Join a channel
    Join {
        /// A channel irc.join allows
        channel: String,
    },
    /// Leave a channel
    Part {
        /// A channel the bot is in
        channel: String,
        /// Why, shown to the channel
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// React to a message with an emoji
    React {
        /// Channel or nick the message was sent to
        target: String,
        /// The msgid of the message to react to
        msgid: String,
        /// A short emoji or word, up to 32 characters
        reaction: String,
        /// Remove this reaction instead of adding it
        #[arg(long)]
        remove: bool,
    },
    /// Show that the bot is typing
    Typing {
        /// Channel or nick to show typing to
        target: String,
        /// `active` while composing, `paused`, or `done` once finished
        state: Typing,
    },
    /// Delete a message the bot sent
    Redact {
        /// Channel or nick the message was sent to
        target: String,
        /// The msgid `louro send` returned
        msgid: String,
        /// Why, shown to clients that support redaction
        #[arg(long, default_value = "")]
        reason: String,
    },
    /// Earlier chat in a channel or DM, from the server
    History {
        /// Channel, or the nick of a DM conversation
        target: String,
        #[command(flatten)]
        #[serde(flatten)]
        query: HistoryQuery,
    },
    /// Look someone up: account, away message, channels, idle time
    Whois {
        /// Their nick
        nick: String,
    },
    /// Who used a nick before, as the server remembers them
    Whowas {
        /// The nick
        nick: String,
    },
    /// Watch a nick: `wait` then reports when it comes online or goes offline
    Watch {
        /// The nick
        nick: String,
        /// Stop watching it instead
        #[arg(long)]
        remove: bool,
    },
    /// Channels on the server, busiest first, with their topics
    List {
        /// Only channels whose name or topic contains this text, ignoring case
        #[arg(long = "match", value_name = "TEXT")]
        pattern: Option<String>,
    },
    /// Disconnect and stop the daemon
    Quit {},
}

/// `none` is `u64::MAX` seconds, past any `Instant`, so the daemon sets no deadline.
fn parse_timeout(raw: &str) -> Result<u64, String> {
    if raw == "none" {
        return Ok(u64::MAX);
    }
    raw.parse()
        .map_err(|_| format!("expected a number of seconds or `none`, got `{raw}`"))
}

/// What the client writes after printing a wait result; only then does the daemon move its cursor.
#[derive(Deserialize, Serialize)]
pub struct Ack {
    pub ack: bool,
}

/// Reads one JSON line, or `None` once the peer has closed the connection.
pub async fn read_line<T: DeserializeOwned>(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> io::Result<Option<T>> {
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&line)?))
}

pub async fn write_line(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &impl Serialize,
) -> io::Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await
}

/// Kept open after `request` so `wait` can ack on the same connection.
pub struct Connection {
    socket: BufReader<UnixStream>,
}

impl Connection {
    pub async fn connect(socket: Option<&Path>) -> Result<Self> {
        let path = resolve_socket_path(socket)?;
        let stream = UnixStream::connect(&path)
            .await
            .map_err(|err| no_session(&path, err))?;
        Ok(Self {
            socket: BufReader::new(stream),
        })
    }

    pub async fn request(&mut self, op: Op) -> Result<Value> {
        write_line(&mut self.socket, &Request { v: VERSION, op })
            .await
            .map_err(louro_stopped)?;
        read_line(&mut self.socket)
            .await
            .map_err(|err| match err.kind() {
                io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => not_louro(),
                _ => louro_stopped(err),
            })?
            .ok_or_else(|| louro_stopped("it closed the connection without answering"))?
    }

    /// Tells the daemon the CLI has printed the last `wait` result, so it
    /// can advance the consumer cursor. Call only after printing.
    pub async fn ack(&mut self) -> Result<()> {
        write_line(&mut self.socket, &Ack { ack: true })
            .await
            .map_err(louro_stopped)
    }
}

pub async fn request(op: Op) -> Result<Value> {
    Connection::connect(None).await?.request(op).await
}

fn no_session(path: &Path, err: impl Display) -> Error {
    Error::new(
        Code::NoSession,
        format!("could not reach louro at {} ({err})", path.display()),
    )
    .with_hint("louro isn't running; ask your operator to start it")
}

/// Whatever listens on the socket answered, but not as louro; its reply is never repeated.
fn not_louro() -> Error {
    Error::new(
        Code::NoSession,
        "the socket's reply is not a louro response",
    )
    .with_hint("ask your operator to check LOURO_SOCKET")
}

/// The daemon was reachable but went away mid-request, e.g. stopped while a `wait` blocked.
fn louro_stopped(err: impl Display) -> Error {
    Error::new(Code::NotConnected, format!("louro stopped ({err})"))
        .with_hint("run `louro status`; if louro isn't running, ask your operator to start it")
}

/// `explicit` (only `connect` has one), else `$LOURO_SOCKET`, else
/// `$XDG_RUNTIME_DIR/louro/louro.sock`, else `$TMPDIR/louro-<uid>/louro.sock`. The agent-facing
/// commands take no path, so the agent can't point louro at another socket.
pub fn resolve_socket_path(explicit: Option<&Path>) -> Result<PathBuf> {
    let chosen = explicit.map(Path::to_path_buf).or_else(|| {
        env::var_os("LOURO_SOCKET")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
    });
    if let Some(path) = chosen {
        return Ok(path);
    }
    if let Some(dir) = env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir).join("louro/louro.sock"));
    }
    let tmp = env::temp_dir();
    Ok(tmp.join(format!("louro-{}/louro.sock", current_uid(&tmp)?)))
}

/// Safe Rust has no getuid, so this reads the owner of a file it creates; `create_new` makes sure
/// that file isn't one someone else left at this guessable name.
fn current_uid(base_dir: &Path) -> Result<u32> {
    let probe = base_dir.join(format!("louro-uid-probe-{}", std::process::id()));
    let uid = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|_| std::fs::metadata(&probe))
        .map(|metadata| metadata.uid());
    let _ = std::fs::remove_file(&probe);
    uid.map_err(|err| {
        Error::new(
            Code::Internal,
            format!("could not determine the user id for the socket path: {err}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::irc::{HistoryQuery, Kind, Outgoing, Typing};

    #[test]
    fn wait_timeout_takes_seconds_or_none() {
        assert_eq!(parse_timeout("60"), Ok(60));
        assert_eq!(parse_timeout("none"), Ok(u64::MAX));
    }

    /// Every `Op`, the ack, and a success and an error response, each pinned to a literal JSON
    /// string. A deliberate wire change updates both this test and `VERSION` in the same commit.
    #[test]
    #[expect(clippy::too_many_lines, reason = "a table with one row per op")]
    fn wire_format_is_pinned() {
        let request = |op: Op| serde_json::to_string(&Request { v: VERSION, op }).unwrap();
        let cases = [
            (
                request(Op::Status {}),
                "{\"v\":6,\"op\":\"status\",\"args\":{}}",
            ),
            (
                request(Op::Wait {
                    timeout: 60,
                    max: NonZeroUsize::new(20).unwrap(),
                }),
                "{\"v\":6,\"op\":\"wait\",\"args\":{\"timeout\":60,\"max\":20}}",
            ),
            (
                request(Op::Send(Outgoing {
                    target: "#bots".into(),
                    kind: Kind::Message,
                    text: "hi there".into(),
                    reply_to: Some("4y8kq7j2c9mnz".into()),
                })),
                "{\"v\":6,\"op\":\"send\",\"args\":{\"target\":\"#bots\",\"kind\":\"message\",\"text\":\"hi there\",\"reply_to\":\"4y8kq7j2c9mnz\"}}",
            ),
            (
                request(Op::Names {
                    channel: "#bots".into(),
                }),
                "{\"v\":6,\"op\":\"names\",\"args\":{\"channel\":\"#bots\"}}",
            ),
            (
                request(Op::Topic {
                    channel: "#bots".into(),
                    set: Some("louro's test channel".into()),
                }),
                "{\"v\":6,\"op\":\"topic\",\"args\":{\"channel\":\"#bots\",\"set\":\"louro's test channel\"}}",
            ),
            (
                request(Op::Join {
                    channel: "#help-rust".into(),
                }),
                "{\"v\":6,\"op\":\"join\",\"args\":{\"channel\":\"#help-rust\"}}",
            ),
            (
                request(Op::Part {
                    channel: "#help-rust".into(),
                    reason: "answered".into(),
                }),
                "{\"v\":6,\"op\":\"part\",\"args\":{\"channel\":\"#help-rust\",\"reason\":\"answered\"}}",
            ),
            (
                request(Op::React {
                    target: "#bots".into(),
                    msgid: "4y8kq7j2c9mnz".into(),
                    reaction: "+1".into(),
                    remove: false,
                }),
                "{\"v\":6,\"op\":\"react\",\"args\":{\"target\":\"#bots\",\"msgid\":\"4y8kq7j2c9mnz\",\"reaction\":\"+1\",\"remove\":false}}",
            ),
            (
                request(Op::Typing {
                    target: "#bots".into(),
                    state: Typing::Active,
                }),
                "{\"v\":6,\"op\":\"typing\",\"args\":{\"target\":\"#bots\",\"state\":\"active\"}}",
            ),
            (
                request(Op::Redact {
                    target: "#bots".into(),
                    msgid: "4y8kq7j2c9mnz".into(),
                    reason: "typo".into(),
                }),
                "{\"v\":6,\"op\":\"redact\",\"args\":{\"target\":\"#bots\",\"msgid\":\"4y8kq7j2c9mnz\",\"reason\":\"typo\"}}",
            ),
            (
                request(Op::History {
                    target: "#bots".into(),
                    query: HistoryQuery {
                        limit: 20,
                        before: Some("4y8kq7j2c9mnz".into()),
                        after: None,
                    },
                }),
                "{\"v\":6,\"op\":\"history\",\"args\":{\"target\":\"#bots\",\"limit\":20,\"before\":\"4y8kq7j2c9mnz\",\"after\":null}}",
            ),
            (
                request(Op::Whois {
                    nick: "luiz".into(),
                }),
                "{\"v\":6,\"op\":\"whois\",\"args\":{\"nick\":\"luiz\"}}",
            ),
            (
                request(Op::Whowas {
                    nick: "luiz".into(),
                }),
                "{\"v\":6,\"op\":\"whowas\",\"args\":{\"nick\":\"luiz\"}}",
            ),
            (
                request(Op::Watch {
                    nick: "luiz".into(),
                    remove: false,
                }),
                "{\"v\":6,\"op\":\"watch\",\"args\":{\"nick\":\"luiz\",\"remove\":false}}",
            ),
            (
                request(Op::List {
                    pattern: Some("rust".into()),
                }),
                "{\"v\":6,\"op\":\"list\",\"args\":{\"pattern\":\"rust\"}}",
            ),
            (
                request(Op::Quit {}),
                "{\"v\":6,\"op\":\"quit\",\"args\":{}}",
            ),
            (
                serde_json::to_string(&Ack { ack: true }).unwrap(),
                "{\"ack\":true}",
            ),
            (
                serde_json::to_string(&Result::<Value>::Ok(
                    serde_json::json!({"sent": true, "target": "#bots"}),
                ))
                .unwrap(),
                "{\"Ok\":{\"sent\":true,\"target\":\"#bots\"}}",
            ),
            (
                serde_json::to_string(&Result::<Value>::Err(
                    Error::new(Code::RateLimited, "too many sends in this window")
                        .with_hint("wait 7 seconds, or don't send")
                        .with_retry_after(7),
                ))
                .unwrap(),
                "{\"Err\":{\"code\":\"rate_limited\",\"message\":\"too many sends in this window\",\"hint\":\"wait 7 seconds, or don't send\",\"retry_after\":7}}",
            ),
        ];
        for (actual, expected) in cases {
            assert_eq!(
                actual, expected,
                "the socket wire format changed: bump socket::VERSION and update this test"
            );
        }
    }
}
