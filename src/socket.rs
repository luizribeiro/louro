use std::env;
use std::fmt::Display;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::UCred;

use crate::config::absolute;
use crate::error::{Code, Error, Result};
use crate::irc::{HistoryQuery, Outgoing, Typing};

pub const VERSION: u8 = 17;

pub const RESTART_HINT: &str = "ask your operator to restart louro with `louro connect --replace`";

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
    /// DM a nick a question and block until they answer or the timeout passes
    Ask(Question),
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
    /// Change the bot's nick
    Nick {
        /// The new nick
        nick: String,
    },
    /// Mark the bot away with a message, or back with --back
    Away {
        /// Why it is away, shown to people who message or look it up
        #[arg(required_unless_present = "back", conflicts_with = "back")]
        text: Option<String>,
        /// No longer away
        #[arg(long)]
        back: bool,
    },
    /// Change the bot's realname, shown in whois
    Setname {
        /// The new realname
        realname: String,
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

/// A question `ask` puts to a nick, who answers with one of `choices`.
#[derive(Deserialize, Serialize, clap::Args)]
pub struct Question {
    /// The nick to ask; irc.ask must grant its account
    pub target: String,
    /// The question, in plain text; louro never cuts it, so too long is refused
    pub text: String,
    /// The answers they may give: 2 to 5 lowercase words
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "yes,no",
        value_name = "A,B,..."
    )]
    pub choices: Vec<String>,
    /// How long to wait for an answer, like `90s` or `5m`, at most 1h
    #[arg(long, default_value = "5m", value_parser = parse_seconds, value_name = "DURATION")]
    pub timeout: u64,
}

fn parse_seconds(raw: &str) -> Result<u64, String> {
    humantime::parse_duration(raw)
        .map(|duration| duration.as_secs())
        .map_err(|_| format!("expected a duration like `90s` or `5m`, got `{raw}`"))
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
    path: PathBuf,
    socket: BufReader<UnixStream>,
}

impl Connection {
    pub async fn connect(socket: Option<&Path>) -> Result<Self> {
        let path = resolve_socket_path(socket)?;
        let stream = UnixStream::connect(&path)
            .await
            .map_err(|err| no_session(&path, err))?;
        Ok(Self {
            path,
            socket: BufReader::new(stream),
        })
    }

    pub fn peer_cred(&self) -> io::Result<UCred> {
        self.socket.get_ref().peer_cred()
    }

    pub async fn request(&mut self, op: Op) -> Result<Value> {
        self.answer(op).await.map_err(louro_stopped)?
    }

    /// The reply to `op`, or why the daemon went away before replying.
    pub async fn answer(&mut self, op: Op) -> io::Result<Result<Value>> {
        write_line(&mut self.socket, &Request { v: VERSION, op }).await?;
        match read_line::<Result<Value>>(&mut self.socket).await {
            Ok(Some(reply)) => Ok(reply.map_err(|err| self.name_socket_in_version_mismatch(err))),
            Ok(None) => Err(io::Error::other(
                "it closed the connection without answering",
            )),
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
                ) =>
            {
                Ok(Err(not_louro()))
            }
            Err(err) => Err(err),
        }
    }

    fn name_socket_in_version_mismatch(&self, err: Error) -> Error {
        if err.code != Code::VersionMismatch {
            return err;
        }
        let hint = err.hint.unwrap_or_else(|| RESTART_HINT.into());
        Error::new(
            Code::VersionMismatch,
            format!(
                "the louro daemon at {} speaks a different protocol version",
                self.path.display()
            ),
        )
        .with_hint(hint)
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
pub fn louro_stopped(err: impl Display) -> Error {
    Error::new(Code::NoSession, format!("louro stopped ({err})")).with_hint(
        "retry; if `louro status` says louro isn't running, ask your operator to start it",
    )
}

/// `explicit` (only `connect` has one), else `$LOURO_SOCKET`, else
/// `$XDG_RUNTIME_DIR/louro/louro.sock`, else `$TMPDIR/louro-<uid>/louro.sock`.
pub fn resolve_socket_path(explicit: Option<&Path>) -> Result<PathBuf> {
    absolute(&chosen_socket_path(explicit))
}

fn chosen_socket_path(explicit: Option<&Path>) -> PathBuf {
    let chosen = explicit.map(Path::to_path_buf).or_else(|| {
        env::var_os("LOURO_SOCKET")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
    });
    if let Some(path) = chosen {
        return path;
    }
    if let Some(dir) = env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir).join("louro/louro.sock");
    }
    let uid = rustix::process::getuid().as_raw();
    env::temp_dir().join(format!("louro-{uid}/louro.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::irc::{HistoryQuery, Kind, Outgoing, Typing};

    #[test]
    fn an_explicit_relative_socket_path_is_made_absolute() {
        let socket = resolve_socket_path(Some(Path::new("run/louro.sock"))).unwrap();
        assert_eq!(socket, env::current_dir().unwrap().join("run/louro.sock"));
    }

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
                "{\"v\":17,\"op\":\"status\",\"args\":{}}",
            ),
            (
                request(Op::Wait {
                    timeout: 60,
                    max: NonZeroUsize::new(20).unwrap(),
                }),
                "{\"v\":17,\"op\":\"wait\",\"args\":{\"timeout\":60,\"max\":20}}",
            ),
            (
                request(Op::Send(Outgoing {
                    target: "#bots".into(),
                    kind: Kind::Message,
                    text: "hi there".into(),
                    reply_to: Some("4y8kq7j2c9mnz".into()),
                })),
                "{\"v\":17,\"op\":\"send\",\"args\":{\"target\":\"#bots\",\"kind\":\"message\",\"text\":\"hi there\",\"reply_to\":\"4y8kq7j2c9mnz\"}}",
            ),
            (
                request(Op::Names {
                    channel: "#bots".into(),
                }),
                "{\"v\":17,\"op\":\"names\",\"args\":{\"channel\":\"#bots\"}}",
            ),
            (
                request(Op::Topic {
                    channel: "#bots".into(),
                    set: Some("louro's test channel".into()),
                }),
                "{\"v\":17,\"op\":\"topic\",\"args\":{\"channel\":\"#bots\",\"set\":\"louro's test channel\"}}",
            ),
            (
                request(Op::Join {
                    channel: "#help-rust".into(),
                }),
                "{\"v\":17,\"op\":\"join\",\"args\":{\"channel\":\"#help-rust\"}}",
            ),
            (
                request(Op::Part {
                    channel: "#help-rust".into(),
                    reason: "answered".into(),
                }),
                "{\"v\":17,\"op\":\"part\",\"args\":{\"channel\":\"#help-rust\",\"reason\":\"answered\"}}",
            ),
            (
                request(Op::React {
                    target: "#bots".into(),
                    msgid: "4y8kq7j2c9mnz".into(),
                    reaction: "+1".into(),
                    remove: false,
                }),
                "{\"v\":17,\"op\":\"react\",\"args\":{\"target\":\"#bots\",\"msgid\":\"4y8kq7j2c9mnz\",\"reaction\":\"+1\",\"remove\":false}}",
            ),
            (
                request(Op::Typing {
                    target: "#bots".into(),
                    state: Typing::Active,
                }),
                "{\"v\":17,\"op\":\"typing\",\"args\":{\"target\":\"#bots\",\"state\":\"active\"}}",
            ),
            (
                request(Op::Ask(Question {
                    target: "luiz".into(),
                    text: "run `rm -rf target`?".into(),
                    choices: vec!["yes".into(), "no".into()],
                    timeout: 300,
                })),
                "{\"v\":17,\"op\":\"ask\",\"args\":{\"target\":\"luiz\",\"text\":\"run `rm -rf target`?\",\"choices\":[\"yes\",\"no\"],\"timeout\":300}}",
            ),
            (
                request(Op::Redact {
                    target: "#bots".into(),
                    msgid: "4y8kq7j2c9mnz".into(),
                    reason: "typo".into(),
                }),
                "{\"v\":17,\"op\":\"redact\",\"args\":{\"target\":\"#bots\",\"msgid\":\"4y8kq7j2c9mnz\",\"reason\":\"typo\"}}",
            ),
            (
                request(Op::History {
                    target: "#bots".into(),
                    query: HistoryQuery {
                        limit: 20,
                        before: Some("4y8kq7j2c9mnz".into()),
                        after: None,
                        grep: Some("(?i)ergo".into()),
                    },
                }),
                "{\"v\":17,\"op\":\"history\",\"args\":{\"target\":\"#bots\",\"limit\":20,\"before\":\"4y8kq7j2c9mnz\",\"after\":null,\"grep\":\"(?i)ergo\"}}",
            ),
            (
                request(Op::Whois {
                    nick: "luiz".into(),
                }),
                "{\"v\":17,\"op\":\"whois\",\"args\":{\"nick\":\"luiz\"}}",
            ),
            (
                request(Op::Whowas {
                    nick: "luiz".into(),
                }),
                "{\"v\":17,\"op\":\"whowas\",\"args\":{\"nick\":\"luiz\"}}",
            ),
            (
                request(Op::Watch {
                    nick: "luiz".into(),
                    remove: false,
                }),
                "{\"v\":17,\"op\":\"watch\",\"args\":{\"nick\":\"luiz\",\"remove\":false}}",
            ),
            (
                request(Op::Nick {
                    nick: "louro_".into(),
                }),
                "{\"v\":17,\"op\":\"nick\",\"args\":{\"nick\":\"louro_\"}}",
            ),
            (
                request(Op::Away {
                    text: Some("thinking".into()),
                    back: false,
                }),
                "{\"v\":17,\"op\":\"away\",\"args\":{\"text\":\"thinking\",\"back\":false}}",
            ),
            (
                request(Op::Setname {
                    realname: "louro, a parrot".into(),
                }),
                "{\"v\":17,\"op\":\"setname\",\"args\":{\"realname\":\"louro, a parrot\"}}",
            ),
            (
                request(Op::List {
                    pattern: Some("rust".into()),
                }),
                "{\"v\":17,\"op\":\"list\",\"args\":{\"pattern\":\"rust\"}}",
            ),
            (
                request(Op::Quit {}),
                "{\"v\":17,\"op\":\"quit\",\"args\":{}}",
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
            (
                serde_json::to_string(&Result::<Value>::Err(
                    Error::new(Code::Blocked, "the filter blocked this text")
                        .with_note("looks like a GitHub token"),
                ))
                .unwrap(),
                "{\"Err\":{\"code\":\"blocked\",\"message\":\"the filter blocked this text\",\"note\":\"looks like a GitHub token\"}}",
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
