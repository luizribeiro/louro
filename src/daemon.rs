use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::sync::Arc;
use std::{fs, io};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, mpsc};

use crate::commands::connect::Args as ConnectArgs;
use crate::error::{Code, Error, Result};
use crate::grants::Grants;
use crate::inbox::Inbox;
use crate::irc::{self, Incoming, Irc, IrcStatus, Kind, Secret};
use crate::output;
use crate::password::{self, Source};
use crate::socket::{Ack, Op, Request, VERSION, read_line, resolve_socket_path, write_line};

struct Daemon<I> {
    grants: Grants,
    irc: I,
    inbox: Inbox,
    quit: Notify,
}

/// Binds the daemon's socket at `path`, replacing a stale one.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    remove_socket(path)?;
    UnixListener::bind(path)
}

/// Serves requests on `listener` and buffers `incoming` chat until a `quit`
/// request, then removes the socket at `socket_path`.
pub async fn run(
    grants: Grants,
    irc: impl Irc,
    mut incoming: mpsc::Receiver<Incoming>,
    listener: UnixListener,
    socket_path: &Path,
) -> io::Result<()> {
    let daemon = Arc::new(Daemon {
        grants,
        irc,
        inbox: Inbox::default(),
        quit: Notify::new(),
    });
    loop {
        tokio::select! {
            Ok((stream, _)) = listener.accept() => {
                let daemon = Arc::clone(&daemon);
                tokio::spawn(async move {
                    let _ = daemon.serve(&mut BufReader::new(stream)).await;
                });
            }
            Some(incoming) = incoming.recv() => daemon.ingest(incoming),
            () = daemon.quit.notified() => break,
        }
    }
    daemon.irc.quit().await;
    remove_socket(socket_path)
}

/// Connects and listens, prints the line `background` waits for, and returns the daemon to run.
pub async fn start(
    args: ConnectArgs,
    socket: Option<&Path>,
) -> Result<impl Future<Output = io::Result<()>>> {
    let socket = resolve_socket_path(socket)?;
    let password = read_password(&args)?;
    create_runtime_dir(&socket)?;
    let (irc, incoming) = irc::connect(&args, password.clone()).await?;
    let listener = bind(&socket).map_err(|err| cannot_listen(&socket, &err))?;
    output::emit(Ok(json!({
        "server": format!("{}:{}", args.server, args.port()),
        "nick": args.nick,
        "channels": args.joins,
        "socket": socket,
    })));
    let grants = Grants {
        channels: args.joins,
        allow_dm: args.allow_dm,
        password,
    };
    Ok(async move { run(grants, irc, incoming, listener, &socket).await })
}

pub fn read_password(args: &ConnectArgs) -> Result<Option<Secret>> {
    password_source(args)
        .map(|source| password::read(&source))
        .transpose()
}

/// A password is only wanted for SASL.
fn password_source(args: &ConnectArgs) -> Option<Source> {
    if args.password_stdin {
        return Some(Source::Stdin);
    }
    if let Some(path) = &args.password_file {
        return Some(Source::File(path.clone()));
    }
    let user = args.sasl_user.as_ref()?;
    Some(Source::Tty(user.clone()))
}

fn create_runtime_dir(socket: &Path) -> Result<()> {
    let Some(dir) = socket.parent() else {
        return Ok(());
    };
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|err| cannot_listen(socket, &err))
}

fn cannot_listen(socket: &Path, err: &io::Error) -> Error {
    Error::new(
        Code::Internal,
        format!("can't listen on {}: {err}", socket.display()),
    )
}

fn remove_socket(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

impl<I: Irc> Daemon<I> {
    /// DMs off means neither sending nor receiving, so an incoming private event never reaches
    /// the inbox.
    fn ingest(&self, incoming: Incoming) {
        if incoming.private && !self.grants.allow_dm {
            return;
        }
        self.inbox.push(incoming);
    }

    async fn serve(&self, conn: &mut BufReader<UnixStream>) -> io::Result<()> {
        let op = match read_line::<Value>(conn).await {
            Ok(Some(request)) => parse_request(request),
            Ok(None) => return Ok(()),
            Err(_) => Err(Error::new(Code::Usage, "the request is not a JSON line")),
        };
        match op {
            Err(error) => reply(conn, Err::<(), _>(error)).await,
            Ok(Op::Status {}) => reply(conn, Ok(self.status())).await,
            Ok(Op::Send { target, text, kind }) => {
                reply(conn, self.send(target, text, kind).await).await
            }
            Ok(Op::Wait { timeout, max }) => {
                let batch = self.inbox.wait(timeout, max).await;
                let cursor = batch.cursor;
                reply(conn, Ok(batch)).await?;
                if let Some(Ack { ack: true }) = read_line(conn).await? {
                    self.inbox.ack(cursor);
                }
                Ok(())
            }
            Ok(Op::Quit {}) => {
                reply(conn, Ok(json!({}))).await?;
                self.quit.notify_one();
                Ok(())
            }
        }
    }

    fn status(&self) -> Value {
        let IrcStatus {
            connected,
            nick,
            channels,
        } = self.irc.status();
        let policy = json!({"channels": self.grants.channels, "dm": self.grants.allow_dm});
        json!({"connected": connected, "nick": nick, "channels": channels, "policy": policy})
    }

    async fn send(&self, target: String, text: String, kind: Kind) -> Result<Value> {
        let message = self.grants.check_send(target, &text, kind)?;
        let (target, lines) = (message.target.clone(), message.text.lines().count());
        let msgid = self.irc.send(message).await?;
        Ok(json!({"target": target, "msgid": msgid, "lines": lines}))
    }
}

fn parse_request(request: Value) -> Result<Op> {
    if request.get("v") != Some(&Value::from(VERSION)) {
        return Err(Error::new(
            Code::VersionMismatch,
            "the louro client and daemon speak different protocol versions",
        )
        .with_hint("restart the daemon with `louro quit` then `louro connect`"));
    }
    serde_json::from_value::<Request>(request)
        .map(|request| request.op)
        .map_err(|error| Error::new(Code::Usage, format!("bad request: {error}")))
}

async fn reply(conn: &mut BufReader<UnixStream>, result: Result<impl Serialize>) -> io::Result<()> {
    write_line(conn, &result).await
}

#[cfg(test)]
mod tests {
    use std::future;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use tempfile::TempDir;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::inbox::NOTICE;
    use crate::irc::{Outgoing, Sender};

    fn from_maria(text: &str) -> Incoming {
        Incoming {
            kind: Kind::Message,
            time: "2026-10-16T21:04:11.392Z".into(),
            msgid: Some("4y8kq7j2c9mnz".into()),
            target: "#bots".into(),
            private: false,
            from: Sender {
                nick: "maria".into(),
                account: Some("maria".into()),
                bot: false,
            },
            text: text.into(),
            mentions_me: true,
            reply_to: None,
        }
    }

    fn from_joao(text: &str) -> Incoming {
        Incoming {
            msgid: Some("9qq1v0x3hdk2a".into()),
            from: Sender {
                nick: "joao".into(),
                account: None,
                bot: false,
            },
            mentions_me: false,
            reply_to: Some("4y8kq7j2c9mnz".into()),
            ..from_maria(text)
        }
    }

    const PASSWORD: &str = "c0rrect-h0rse-battery";

    fn grants() -> Grants {
        Grants {
            channels: vec!["#bots".into(), "#ergo".into()],
            allow_dm: false,
            password: Some(PASSWORD.into()),
        }
    }

    #[derive(Default)]
    struct FakeIrc {
        sent: Mutex<Vec<Outgoing>>,
        quit: Notify,
    }

    impl Irc for Arc<FakeIrc> {
        fn status(&self) -> IrcStatus {
            IrcStatus {
                connected: true,
                nick: "louro".into(),
                channels: vec!["#bots".into(), "#ergo".into()],
            }
        }

        fn send(&self, message: Outgoing) -> impl Future<Output = Result<Option<String>>> + Send {
            self.sent.lock().unwrap().push(message);
            future::ready(Ok(Some("c2m8w1u0pq7rs".into())))
        }

        async fn quit(&self) {
            self.quit.notify_one();
        }
    }

    struct Running {
        _dir: TempDir,
        socket: PathBuf,
        chat: mpsc::Sender<Incoming>,
        irc: Arc<FakeIrc>,
        daemon: JoinHandle<io::Result<()>>,
    }

    fn start() -> Running {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("louro.sock");
        let irc = Arc::new(FakeIrc::default());
        let (chat, incoming) = mpsc::channel(16);
        let daemon = tokio::spawn({
            let irc = Arc::clone(&irc);
            let socket = socket.clone();
            async move { run(grants(), irc, incoming, bind(&socket)?, &socket).await }
        });
        Running {
            _dir: dir,
            socket,
            chat,
            irc,
            daemon,
        }
    }

    impl Running {
        async fn connect(&self) -> BufReader<UnixStream> {
            for _ in 0..1000 {
                if let Ok(stream) = UnixStream::connect(&self.socket).await {
                    return BufReader::new(stream);
                }
                tokio::task::yield_now().await;
            }
            panic!("the daemon never listened on {}", self.socket.display());
        }

        async fn call(&self, request: Value) -> Value {
            let mut conn = self.connect().await;
            write_line(&mut conn, &request).await.unwrap();
            read_line(&mut conn).await.unwrap().unwrap()
        }

        async fn wait(&self, timeout_secs: u64) -> (BufReader<UnixStream>, Value) {
            let mut conn = self.connect().await;
            write_line(&mut conn, &wait_request(timeout_secs))
                .await
                .unwrap();
            let reply = read_line(&mut conn).await.unwrap().unwrap();
            (conn, reply)
        }

        async fn say(&self, incoming: Incoming) {
            self.chat.send(incoming).await.unwrap();
        }
    }

    fn wait_request(timeout_secs: u64) -> Value {
        json!({"v": 1, "op": "wait", "args": {"timeout": timeout_secs, "max": 20}})
    }

    fn send_request(target: &str, text: &str) -> Value {
        json!({"v": 1, "op": "send", "args": {"target": target, "text": text, "kind": "message"}})
    }

    fn texts(reply: &Value) -> Vec<&str> {
        reply["Ok"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["text"].as_str().unwrap())
            .collect()
    }

    #[tokio::test]
    async fn status_reports_the_connection_and_the_policy() {
        let daemon = start();
        let reply = daemon
            .call(json!({"v": 1, "op": "status", "args": {}}))
            .await;
        assert_eq!(
            reply,
            json!({"Ok": {
                "connected": true,
                "nick": "louro",
                "channels": ["#bots", "#ergo"],
                "policy": {"channels": ["#bots", "#ergo"], "dm": false}
            }})
        );
    }

    #[tokio::test]
    async fn a_wait_gets_sanitized_chat_that_arrives_while_waiting() {
        let daemon = start();
        let waiting = tokio::spawn({
            let mut conn = daemon.connect().await;
            async move {
                write_line(&mut conn, &wait_request(30)).await.unwrap();
                read_line::<Value>(&mut conn).await.unwrap().unwrap()
            }
        });
        daemon
            .say(from_maria(
                "louro\0: does ergo support chathistory for DMs?",
            ))
            .await;

        let reply = waiting.await.unwrap();
        assert!(reply["Ok"].is_object());
        assert_eq!(reply["Ok"]["timed_out"], false);
        assert_eq!(reply["Ok"]["notice"], NOTICE);
        assert_eq!(reply["Ok"]["events"][0]["untrusted"], true);
        assert_eq!(
            texts(&reply),
            ["louro: does ergo support chathistory for DMs?"]
        );
    }

    #[tokio::test]
    async fn a_dm_is_dropped_when_dms_are_off() {
        let daemon = start();
        daemon
            .say(Incoming {
                private: true,
                target: "louro".into(),
                ..from_maria("hey, got a sec?")
            })
            .await;
        daemon.say(from_joao("louro: any news?")).await;

        let (_, reply) = daemon.wait(5).await;
        assert_eq!(texts(&reply), ["louro: any news?"]);
    }

    #[tokio::test]
    async fn events_come_back_until_the_client_acks() {
        let daemon = start();
        daemon
            .say(from_maria("louro: does ergo support chathistory for DMs?"))
            .await;
        daemon.say(from_joao("it does, it's a config option")).await;

        let (unacked, first) = daemon.wait(5).await;
        drop(unacked);
        let (mut conn, again) = daemon.wait(5).await;
        assert_eq!(texts(&again), texts(&first));
        assert_eq!(texts(&again).len(), 2);

        write_line(&mut conn, &Ack { ack: true }).await.unwrap();
        assert_eq!(read_line::<Value>(&mut conn).await.unwrap(), None);
        let (_, after_ack) = daemon.wait(0).await;
        assert_eq!(after_ack["Ok"]["timed_out"], true);
        assert_eq!(after_ack["Ok"]["cursor"], 2);
    }

    #[tokio::test]
    async fn send_passes_a_cleaned_message_to_irc() {
        let daemon = start();
        let reply = daemon
            .call(send_request(
                "#bots",
                "yes: set history.persistent.direct-messages\r\nin ergo.yaml\n",
            ))
            .await;
        assert_eq!(
            reply,
            json!({"Ok": {
                "target": "#bots", "msgid": "c2m8w1u0pq7rs", "lines": 2
            }})
        );
        assert_eq!(
            *daemon.irc.sent.lock().unwrap(),
            [Outgoing {
                target: "#bots".into(),
                kind: Kind::Message,
                text: "yes: set history.persistent.direct-messages \nin ergo.yaml".into(),
            }]
        );
    }

    #[tokio::test]
    async fn a_refused_send_never_reaches_irc_or_echoes_the_password() {
        let daemon = start();
        let leak = daemon
            .call(send_request("#bots", &format!("sure, it's {PASSWORD}")))
            .await;
        assert!(leak["Ok"].is_null());
        assert_eq!(leak["Err"]["code"], "blocked_secret");
        assert!(!leak.to_string().contains(PASSWORD));

        let elsewhere = daemon.call(send_request("#lounge", "hi all")).await;
        assert_eq!(elsewhere["Err"]["code"], "not_allowed");
        assert!(daemon.irc.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn another_protocol_version_is_a_mismatch() {
        let reply = start()
            .call(json!({"v": 2, "op": "history", "args": {"target": "#bots"}}))
            .await;
        assert_eq!(reply["Err"]["code"], "version_mismatch");
    }

    #[tokio::test]
    async fn quit_replies_then_shuts_down_and_removes_the_socket() {
        let daemon = start();
        let reply = daemon.call(json!({"v": 1, "op": "quit", "args": {}})).await;
        assert_eq!(reply, json!({"Ok": {}}));

        daemon.daemon.await.unwrap().unwrap();
        daemon.irc.quit.notified().await;
        assert!(!daemon.socket.exists());
    }
}
