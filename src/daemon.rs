use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::fs::DirBuilder;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use std::{fs, io};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;

use crate::config::{Config, Grants, Limit, Limits};
use crate::error::{Code, Error, Result};
use crate::grants::DmHistory;
use crate::inbox::{Connection, ConnectionState, Inbox, Invite, NOTICE};
use crate::irc::{
    self, History, HistoryQuery, Incoming, Irc, Member, Outgoing, Secret, Sender, Typing, Update,
    whole_seconds,
};
use crate::output;
use crate::socket::{Ack, Op, Request, VERSION, read_line, resolve_socket_path, write_line};
use crate::text;

/// The most a `history` call returns, and Ergo's default page size.
const MAX_HISTORY: usize = 100;
const SENT_LOG_CAP: usize = 1_000;

/// The bot's own recent messages as (lowercased target, msgid), the only ones `redact` removes.
#[derive(Default)]
struct SentLog(VecDeque<(String, String)>);

impl SentLog {
    fn record(&mut self, target: &str, msgid: String) {
        if self.0.len() == SENT_LOG_CAP {
            self.0.pop_front();
        }
        self.0.push_back((target.to_ascii_lowercase(), msgid));
    }

    fn contains(&self, target: &str, msgid: &str) -> bool {
        self.0
            .iter()
            .any(|(sent_to, id)| id == msgid && sent_to.eq_ignore_ascii_case(target))
    }
}

/// The most recent thing that went wrong with the connection, for `status`.
#[derive(Serialize)]
struct Problem {
    time: String,
    message: String,
}

/// Sliding windows of recent attempts, one deque per lowercased key (a send/react target, or a
/// fixed key for a limit that isn't per-target), trimmed to the window on every check.
#[derive(Default)]
struct RateWindows(Mutex<HashMap<String, VecDeque<Instant>>>);

impl RateWindows {
    /// Checks `key` against `limit` and, if it has room, immediately reserves a slot by
    /// recording `now` — so a concurrent attempt can't also pass the check while this one is
    /// still in flight. `None` on success (never refused for [`Limit::Off`], which never
    /// reserves either); otherwise the whole seconds, rounded up, until the oldest entry leaves
    /// the window. Undo a reservation that doesn't go through with [`Self::release`].
    fn reserve(&self, key: &str, limit: Limit, now: Instant) -> Option<u64> {
        let Limit::Window { count, seconds } = limit else {
            return None;
        };
        let window = Duration::from_secs(seconds);
        let mut windows = self.0.lock().unwrap();
        let entries = windows.entry(key.to_ascii_lowercase()).or_default();
        while entries.front().is_some_and(|&t| now - t >= window) {
            entries.pop_front();
        }
        if entries.len() >= count as usize {
            let elapsed = now - *entries.front().unwrap();
            return Some(whole_seconds(window.saturating_sub(elapsed)));
        }
        entries.push_back(now);
        None
    }

    fn release(&self, key: &str, now: Instant) {
        if let Some(entries) = self.0.lock().unwrap().get_mut(&key.to_ascii_lowercase())
            && let Some(pos) = entries.iter().position(|&t| t == now)
        {
            entries.remove(pos);
        }
    }
}

struct Daemon<I> {
    grants: Grants,
    limits: Limits,
    /// Kept so `send` can refuse to post it.
    password: Option<Secret>,
    irc: I,
    inbox: Inbox,
    /// By lowercased nick, what DMs that reached the agent say, for `$first` and `account:`.
    dm_senders: Mutex<HashMap<String, DmHistory>>,
    sent: Mutex<SentLog>,
    last_error: Mutex<Option<Problem>>,
    /// Set from a disconnect until the next registration, so the agent hears of each outage once.
    offline: AtomicBool,
    quit: Notify,
    send_windows: RateWindows,
    send_total: RateWindows,
    react_windows: RateWindows,
}

/// Serves requests on `listener` and buffers chat from `updates` until a `quit`
/// request or `stop`, then quits IRC and removes the socket at `socket_path`. Fails once papo
/// gives up reconnecting, so a supervisor can restart louro.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    grants: Grants,
    limits: Limits,
    password: Option<Secret>,
    irc: impl Irc,
    mut updates: mpsc::Receiver<Update>,
    listener: UnixListener,
    socket_path: &Path,
    stop: impl Future<Output = ()>,
) -> io::Result<()> {
    let daemon = Arc::new(Daemon {
        grants,
        limits,
        password,
        irc,
        inbox: Inbox::default(),
        dm_senders: Mutex::default(),
        sent: Mutex::default(),
        last_error: Mutex::default(),
        offline: AtomicBool::new(false),
        quit: Notify::new(),
        send_windows: RateWindows::default(),
        send_total: RateWindows::default(),
        react_windows: RateWindows::default(),
    });
    tokio::pin!(stop);
    let outcome = loop {
        tokio::select! {
            Ok((stream, _)) = listener.accept() => {
                let daemon = Arc::clone(&daemon);
                tokio::spawn(async move {
                    let _ = daemon.serve(&mut BufReader::new(stream)).await;
                });
            }
            update = updates.recv() => match update {
                Some(update) => daemon.update(update),
                None => break Err(daemon.gave_up()),
            },
            () = daemon.quit.notified() => break Ok(()),
            () = &mut stop => break Ok(()),
        }
    };
    daemon.irc.quit().await;
    remove_socket(socket_path)?;
    outcome
}

/// Connects and listens, prints the line `background` waits for, and returns the daemon to run.
pub async fn start(
    config: Config,
    password: Option<Secret>,
) -> Result<impl Future<Output = io::Result<()>>> {
    let Config {
        server,
        grants,
        limits,
    } = config;
    let socket = resolve_socket_path(server.socket.as_deref())?;
    create_runtime_dir(&socket)?;
    remove_socket(&socket).map_err(|err| cannot_listen(&socket, &err))?;
    let (irc, updates) = irc::connect(&server, password.clone()).await?;
    let listener = UnixListener::bind(&socket).map_err(|err| cannot_listen(&socket, &err))?;
    let stop = stop_signal()
        .map_err(|err| Error::new(Code::Internal, format!("can't handle stop signals: {err}")))?;
    output::emit(Ok(json!({
        "server": server.address,
        "nick": server.nick,
        "channels": server.autojoin,
        "socket": socket,
    })));
    Ok(async move {
        run(
            grants, limits, password, irc, updates, listener, &socket, stop,
        )
        .await
    })
}

/// SIGTERM (a service manager) and SIGINT (Ctrl-C in the foreground) stop louro like `quit`.
fn stop_signal() -> io::Result<impl Future<Output = ()>> {
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
    })
}

pub fn already_running() -> Error {
    Error::new(Code::SessionExists, "louro is already running").with_hint("run `louro quit` first")
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

/// Removes a socket at `path`, such as one a crashed louro left; anything else there, a symlink
/// included, is refused, since `connect` may be pointed at any path.
fn remove_socket(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "something other than a socket is there",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

impl<I: Irc> Daemon<I> {
    fn update(&self, update: Update) {
        match update {
            Update::Chat(incoming) => self.ingest(incoming),
            Update::Disconnected { cause, retry_in } => {
                *self.last_error.lock().unwrap() = Some(Problem {
                    time: now(),
                    message: disconnect_message(&cause, retry_in),
                });
                if retry_in.is_some() && !self.offline.swap(true, Ordering::Relaxed) {
                    self.report_connection(ConnectionState::Disconnected, retry_in);
                }
            }
            Update::Registered => {
                if self.offline.swap(false, Ordering::Relaxed) {
                    self.report_connection(ConnectionState::Reconnected, None);
                }
            }
            Update::Invited { from, channel } => self.inbox.push_invite(Invite {
                time: now(),
                from,
                channel,
            }),
        }
    }

    fn report_connection(&self, state: ConnectionState, retry_in: Option<u64>) {
        self.inbox.push_connection(Connection {
            time: now(),
            state,
            retry_in,
        });
    }

    fn gave_up(&self) -> io::Error {
        let reason = self.last_error.lock().unwrap().as_ref().map_or_else(
            || "papo's event stream ended".into(),
            |problem| problem.message.clone(),
        );
        io::Error::other(reason)
    }

    fn ingest(&self, incoming: Incoming) {
        if !self.grants.may_read(&incoming) {
            return;
        }
        if incoming.private {
            self.record_dm_sender(&incoming.from);
        }
        self.inbox.push(incoming);
    }

    /// Once a nick's DMs carried an account, a DM with another one (or none) means the nick may
    /// have changed hands, so it loses `$first` and its account for the rest of the daemon's run.
    fn record_dm_sender(&self, from: &Sender) {
        let nick = from.nick.to_ascii_lowercase();
        let account = from
            .account
            .as_ref()
            .map(|account| account.to_ascii_lowercase());
        match self.dm_senders.lock().unwrap().entry(nick) {
            Entry::Occupied(mut sender) => {
                let sender = sender.get_mut();
                if sender.account.is_some() && sender.account != account {
                    *sender = DmHistory::default();
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(DmHistory {
                    first: true,
                    account,
                });
            }
        }
    }

    /// What's known about DMs from `target`, for `$first` and `account:` entries in
    /// `irc.dm.send`.
    fn dm_history(&self, target: &str) -> DmHistory {
        self.dm_senders
            .lock()
            .unwrap()
            .get(&target.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default()
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
            Ok(Op::Send(message)) => reply(conn, self.send(message).await).await,
            Ok(Op::Wait { timeout, max }) => {
                let batch = self.inbox.wait(timeout, max).await;
                let cursor = batch.cursor;
                reply(conn, Ok(batch)).await?;
                if let Some(Ack { ack: true }) = read_line(conn).await? {
                    self.inbox.ack(cursor);
                }
                Ok(())
            }
            Ok(Op::Names { channel }) => reply(conn, self.names(&channel)).await,
            Ok(Op::React {
                target,
                msgid,
                reaction,
                remove,
            }) => {
                let reacted = self.react(&target, &msgid, &reaction, remove).await;
                reply(conn, reacted).await
            }
            Ok(Op::Typing { target, state }) => {
                reply(conn, self.typing(&target, state).await).await
            }
            Ok(Op::Redact {
                target,
                msgid,
                reason,
            }) => reply(conn, self.redact(&target, &msgid, &reason).await).await,
            Ok(Op::History { target, query }) => {
                reply(conn, self.history(&target, query).await).await
            }
            Ok(Op::Quit {}) => {
                let allowed = self.grants.check_quit();
                let quitting = allowed.is_ok();
                reply(conn, allowed.map(|()| json!({}))).await?;
                if quitting {
                    self.quit.notify_one();
                }
                Ok(())
            }
        }
    }

    fn status(&self) -> Value {
        let mut status = json!(self.irc.status());
        status["grants"] = json!(self.grants);
        status["limits"] = json!(self.limits);
        status["last_error"] = json!(*self.last_error.lock().unwrap());
        status
    }

    fn names(&self, channel: &str) -> Result<Value> {
        self.grants.check_join(channel)?;
        let members: Vec<Member> = self
            .irc
            .names(channel)
            .ok_or_else(|| {
                Error::new(Code::NotAllowed, format!("louro is not in {channel}"))
                    .with_hint("`louro status` lists the channels it is in")
            })?
            .into_iter()
            .map(|member| Member {
                user: member.user.sanitized(),
                ..member
            })
            .collect();
        Ok(json!({"channel": channel, "members": members}))
    }

    async fn react(
        &self,
        target: &str,
        msgid: &str,
        reaction: &str,
        remove: bool,
    ) -> Result<Value> {
        text::check_secret(self.password.as_ref(), reaction)?;
        let dm = self.dm_history(target);
        self.grants
            .check_also(&self.grants.irc.react, "irc.react", target, &dm)?;
        text::check_msgid(msgid)?;
        text::check_reaction(reaction)?;
        self.check_react_budget(target)?;
        self.irc.react(target, msgid, reaction, remove).await?;
        Ok(json!({"target": target, "msgid": msgid, "reaction": reaction, "removed": remove}))
    }

    /// `Err` once `target` has reached `limits.react` within its window; a refused attempt
    /// never touches the budget.
    fn check_react_budget(&self, target: &str) -> Result<()> {
        let now = Instant::now();
        if let Some(retry_after) = self.react_windows.reserve(target, self.limits.react, now) {
            return Err(rate_limited("react", retry_after));
        }
        Ok(())
    }

    /// Never touches the inbox, so `wait`'s cursor stays where it was.
    async fn history(&self, target: &str, query: HistoryQuery) -> Result<Value> {
        self.grants
            .check_history(target, &self.dm_history(target))?;
        let query = HistoryQuery {
            limit: query.limit.clamp(1, MAX_HISTORY),
            ..query
        };
        let History { events, complete } = self.irc.history(target, query).await?;
        let events: Vec<_> = events
            .into_iter()
            .filter(|incoming| incoming.from_me || self.grants.may_read(incoming))
            .map(Inbox::sanitized)
            .collect();
        Ok(json!({
            "target": target,
            "complete": complete,
            "notice": NOTICE,
            "events": events,
        }))
    }

    async fn redact(&self, target: &str, msgid: &str, reason: &str) -> Result<Value> {
        self.grants.check_redact()?;
        text::check_msgid(msgid)?;
        text::check_one_line(reason)?;
        text::check_secret(self.password.as_ref(), reason)?;
        if !self.sent.lock().unwrap().contains(target, msgid) {
            return Err(Error::new(
                Code::NotAllowed,
                format!("{msgid} in {target} is not a message louro sent"),
            )
            .with_hint("louro redacts only its own messages, sent since it started"));
        }
        self.irc.redact(target, msgid, reason).await?;
        Ok(json!({"target": target, "msgid": msgid}))
    }

    async fn typing(&self, target: &str, state: Typing) -> Result<Value> {
        let dm = self.dm_history(target);
        self.grants
            .check_also(&self.grants.irc.typing, "irc.typing", target, &dm)?;
        self.irc.typing(target, state).await?;
        Ok(json!({"target": target, "state": state}))
    }

    async fn send(&self, message: Outgoing) -> Result<Value> {
        text::check_secret(self.password.as_ref(), &message.text)?;
        let dm = self.dm_history(&message.target);
        self.grants.check_send(&message.target, &dm)?;
        if let Some(msgid) = &message.reply_to {
            text::check_msgid(msgid)?;
        }
        let message = Outgoing {
            text: text::clean_lines(&message.text)?,
            ..message
        };
        let now = self.reserve_send_budget(&message.target)?;
        let (target, lines) = (message.target.clone(), message.text.lines().count());
        let is_reply = message.reply_to.is_some();
        let sent = match self.irc.send(message).await {
            Ok(sent) => sent,
            Err(error) => {
                self.release_send_budget(&target, now);
                return Err(error);
            }
        };
        let mut result = json!({"sent": true, "target": target, "lines": lines});
        if let Some(msgid) = sent.msgid {
            self.sent.lock().unwrap().record(&target, msgid.clone());
            result["msgid"] = msgid.into();
        }
        if is_reply {
            result["threaded"] = sent.threaded.into();
        }
        Ok(result)
    }

    /// `Err` once `target` has reached `limits.send`, or any target has reached
    /// `limits.send_total`, within its window; a refused send never touches either budget. A
    /// reservation this returns must be undone with [`Self::release_send_budget`] if the send it
    /// was held for never reaches IRC, so a send that fails doesn't spend budget it didn't use.
    fn reserve_send_budget(&self, target: &str) -> Result<Instant> {
        let now = Instant::now();
        if let Some(retry_after) = self.send_windows.reserve(target, self.limits.send, now) {
            return Err(rate_limited("send", retry_after));
        }
        if let Some(retry_after) = self.send_total.reserve(TOTAL, self.limits.send_total, now) {
            self.send_windows.release(target, now);
            return Err(rate_limited("send", retry_after));
        }
        Ok(now)
    }

    fn release_send_budget(&self, target: &str, now: Instant) {
        self.send_windows.release(target, now);
        self.send_total.release(TOTAL, now);
    }
}

/// The one key `send_total`'s [`RateWindows`] ever uses: it isn't per-target.
const TOTAL: &str = "";

fn rate_limited(action: &str, retry_after: u64) -> Error {
    Error::new(
        Code::RateLimited,
        format!("too many {action}s in this window"),
    )
    .with_hint(format!("wait {retry_after} seconds, or don't {action}"))
    .with_retry_after(retry_after)
}

fn now() -> String {
    humantime::format_rfc3339_seconds(SystemTime::now()).to_string()
}

fn disconnect_message(cause: &str, retry_in: Option<u64>) -> String {
    match retry_in {
        Some(seconds) => format!("{cause}; reconnecting in {seconds}s"),
        None => format!("{cause}; papo stopped reconnecting"),
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
    use std::sync::atomic::AtomicUsize;

    use tempfile::TempDir;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::irc::fixtures::{dm, from_joao, from_luiz, from_maria, sender};
    use crate::irc::{IrcStatus, Kind, Origin, Sender, Sent};

    const PASSWORD: &str = "c0rrect-h0rse-battery";

    fn grants() -> Grants {
        toml::from_str(
            r##"
            irc.join = ["#bots", "#lounge"]
            irc.send = ["#bots"]
            irc.dm.read = ["luiz", "maria"]
            irc.dm.send = ["$first"]
            irc.react = ["#bots"]
            irc.typing = ["#bots"]
            irc.redact = true
            "##,
        )
        .unwrap()
    }

    #[derive(Default)]
    struct FakeIrc {
        sent: Mutex<Vec<Outgoing>>,
        /// Whether each reaction asked for was a removal.
        reactions: Mutex<Vec<bool>>,
        typing: Mutex<Vec<Typing>>,
        history_limits: Mutex<Vec<usize>>,
        redacted: Mutex<Vec<String>>,
        /// Sends get no msgid, as from a server without echo-message.
        without_msgid: AtomicBool,
        /// How many of the next sends fail with `not_connected`, as while IRC is down.
        fail_sends: AtomicUsize,
        quit: Notify,
    }

    impl Irc for Arc<FakeIrc> {
        fn status(&self) -> IrcStatus {
            IrcStatus {
                connected: true,
                nick: "louro".into(),
                channels: vec!["#bots".into(), "#lounge".into()],
            }
        }

        fn names(&self, channel: &str) -> Option<Vec<Member>> {
            (channel == "#bots").then(|| {
                vec![
                    Member {
                        user: from_luiz("").from,
                        prefix: "@".into(),
                    },
                    Member {
                        user: Sender {
                            nick: "mal\u{1b}[2Jlory".into(),
                            account: Some("mal\u{7}lory".into()),
                            away: true,
                            bot: true,
                        },
                        prefix: String::new(),
                    },
                ]
            })
        }

        fn send(&self, message: Outgoing) -> impl Future<Output = Result<Sent>> + Send {
            let remaining = self.fail_sends.load(Ordering::Relaxed);
            if remaining > 0 {
                self.fail_sends.store(remaining - 1, Ordering::Relaxed);
                return future::ready(Err(Error::new(Code::NotConnected, "louro stopped")));
            }
            let threaded = message.reply_to.is_some();
            self.sent.lock().unwrap().push(message);
            let msgid =
                (!self.without_msgid.load(Ordering::Relaxed)).then(|| "c2m8w1u0pq7rs".into());
            future::ready(Ok(Sent { msgid, threaded }))
        }

        fn react(
            &self,
            _target: &str,
            _msgid: &str,
            _reaction: &str,
            remove: bool,
        ) -> impl Future<Output = Result<()>> + Send {
            self.reactions.lock().unwrap().push(remove);
            future::ready(Ok(()))
        }

        fn typing(&self, _target: &str, state: Typing) -> impl Future<Output = Result<()>> + Send {
            self.typing.lock().unwrap().push(state);
            future::ready(Ok(()))
        }

        fn history(
            &self,
            _target: &str,
            query: HistoryQuery,
        ) -> impl Future<Output = Result<History>> + Send {
            self.history_limits.lock().unwrap().push(query.limit);
            let history = |incoming| Incoming {
                origin: Origin::History,
                ..incoming
            };
            let own = Incoming {
                from: Sender {
                    nick: "louro".into(),
                    account: Some("louro".into()),
                    away: false,
                    bot: true,
                },
                from_me: true,
                ..from_maria("it's in ergo.yaml")
            };
            future::ready(Ok(History {
                events: vec![
                    history(from_maria("louro\0: where is that set?")),
                    history(own),
                    history(Incoming {
                        target: "#ops".into(),
                        ..from_joao("not for you")
                    }),
                ],
                complete: true,
            }))
        }

        fn redact(
            &self,
            _target: &str,
            msgid: &str,
            _reason: &str,
        ) -> impl Future<Output = Result<()>> + Send {
            self.redacted.lock().unwrap().push(msgid.into());
            future::ready(Ok(()))
        }

        async fn quit(&self) {
            self.quit.notify_one();
        }
    }

    struct Running {
        _dir: TempDir,
        socket: PathBuf,
        updates: mpsc::Sender<Update>,
        irc: Arc<FakeIrc>,
        stop: Option<oneshot::Sender<()>>,
        daemon: JoinHandle<io::Result<()>>,
    }

    fn launch(grants: Grants) -> Running {
        launch_with(grants, Limits::default())
    }

    fn launch_with(grants: Grants, limits: Limits) -> Running {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("louro.sock");
        let irc = Arc::new(FakeIrc::default());
        let (updates, receiver) = mpsc::channel(16);
        let (stop, stopped) = oneshot::channel();
        let daemon = tokio::spawn({
            let irc = Arc::clone(&irc);
            let socket = socket.clone();
            let password = Some(PASSWORD.into());
            let stopped = async {
                let _ = stopped.await;
            };
            async move {
                let listener = UnixListener::bind(&socket)?;
                run(
                    grants, limits, password, irc, receiver, listener, &socket, stopped,
                )
                .await
            }
        });
        Running {
            _dir: dir,
            socket,
            updates,
            irc,
            stop: Some(stop),
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
            self.update(Update::Chat(incoming)).await;
        }

        async fn update(&self, update: Update) {
            self.updates.send(update).await.unwrap();
        }

        async fn status(&self) -> Value {
            self.call(json!({"v": 1, "op": "status", "args": {}})).await
        }
    }

    fn wait_request(timeout_secs: u64) -> Value {
        json!({"v": 1, "op": "wait", "args": {"timeout": timeout_secs, "max": 20}})
    }

    fn send_request(target: &str, text: &str) -> Value {
        json!({"v": 1, "op": "send", "args": {"target": target, "text": text, "kind": "message"}})
    }

    fn reply_request(target: &str, msgid: &str, text: &str) -> Value {
        let mut request = send_request(target, text);
        request["args"]["reply_to"] = msgid.into();
        request
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
    async fn status_reports_the_connection_and_the_grants() {
        let daemon = launch(grants());
        let reply = daemon.status().await;
        assert_eq!(
            reply,
            json!({"Ok": {
                "connected": true,
                "nick": "louro",
                "channels": ["#bots", "#lounge"],
                "grants": {"irc": {
                    "join": ["#bots", "#lounge"],
                    "send": ["#bots"],
                    "dm": {"read": ["luiz", "maria"], "send": ["$first"]},
                    "react": ["#bots"],
                    "typing": ["#bots"],
                    "redact": true,
                    "quit": false
                }},
                "limits": {"send": "3/10s", "send_total": "10/60s", "react": "10/60s"},
                "last_error": null
            }})
        );
    }

    #[tokio::test]
    async fn status_reports_the_last_disconnect() {
        let daemon = launch(grants());
        for (cause, retry_in) in [
            ("the connection was lost", Some(1)),
            ("the server stopped responding", Some(4)),
            ("the connection was lost", None),
        ] {
            daemon
                .update(Update::Disconnected {
                    cause: cause.into(),
                    retry_in,
                })
                .await;
        }
        let last_error = &daemon.status().await["Ok"]["last_error"];
        assert_eq!(
            last_error["message"],
            "the connection was lost; papo stopped reconnecting"
        );
        assert!(last_error["time"].as_str().unwrap().ends_with('Z'));
    }

    #[tokio::test]
    async fn wait_reports_each_outage_once_whatever_the_grants() {
        let daemon = launch(Grants::default());
        daemon.update(Update::Registered).await;
        for retry_in in [Some(1), Some(2), Some(4)] {
            daemon
                .update(Update::Disconnected {
                    cause: "the connection was lost".into(),
                    retry_in,
                })
                .await;
        }
        daemon.update(Update::Registered).await;

        let (_, reply) = daemon.wait(5).await;
        let events = reply["Ok"]["events"].as_array().unwrap();
        let states: Vec<_> = events
            .iter()
            .map(|event| (&event["type"], &event["state"], &event["retry_in"]))
            .collect();
        assert_eq!(
            states,
            [
                (&json!("connection"), &json!("disconnected"), &json!(1)),
                (&json!("connection"), &json!("reconnected"), &Value::Null),
            ]
        );
        assert!(events.iter().all(|event| event.get("untrusted").is_none()));
    }

    #[tokio::test]
    async fn a_wait_gets_sanitized_chat_that_arrives_while_waiting() {
        let daemon = launch(grants());
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
    async fn wait_gets_only_granted_channels_and_dms() {
        let daemon = launch(grants());
        daemon.say(dm(from_joao("hey, got a sec?"))).await;
        daemon
            .say(Incoming {
                target: "#ops".into(),
                ..from_maria("louro: you there?")
            })
            .await;
        daemon.say(from_joao("louro: any news?")).await;
        daemon.say(dm(from_luiz("status report, please"))).await;

        let (_, reply) = daemon.wait(5).await;
        assert_eq!(texts(&reply), ["louro: any news?", "status report, please"]);
    }

    #[tokio::test]
    async fn history_gets_the_same_grants_and_sanitizing_as_live_chat() {
        let daemon = launch(grants());
        let history = |incoming| Incoming {
            origin: Origin::History,
            ..incoming
        };
        daemon
            .say(history(Incoming {
                target: "#ops".into(),
                ..from_maria("louro: you there?")
            }))
            .await;
        daemon.say(history(dm(from_joao("hey, got a sec?")))).await;
        daemon.say(history(from_maria("louro\0: missed me?"))).await;

        let (_, reply) = daemon.wait(5).await;
        assert_eq!(texts(&reply), ["louro: missed me?"]);
        let event = &reply["Ok"]["events"][0];
        assert_eq!(event["origin"], "history");
        assert_eq!(event["untrusted"], true);
    }

    #[tokio::test]
    async fn events_come_back_until_the_client_acks() {
        let daemon = launch(grants());
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
        let daemon = launch(grants());
        let reply = daemon
            .call(send_request(
                "#bots",
                "yes: set history.persistent.direct-messages\r\nin ergo.yaml\n",
            ))
            .await;
        assert_eq!(
            reply,
            json!({"Ok": {
                "sent": true, "target": "#bots", "msgid": "c2m8w1u0pq7rs", "lines": 2
            }})
        );
        assert_eq!(
            *daemon.irc.sent.lock().unwrap(),
            [Outgoing {
                target: "#bots".into(),
                kind: Kind::Message,
                text: "yes: set history.persistent.direct-messages \nin ergo.yaml".into(),
                reply_to: None,
            }]
        );
    }

    #[tokio::test]
    async fn a_send_without_a_msgid_still_says_it_was_sent() {
        let daemon = launch(grants());
        daemon.irc.without_msgid.store(true, Ordering::Relaxed);
        let reply = daemon.call(send_request("#bots", "on it")).await;
        assert_eq!(
            reply,
            json!({"Ok": {"sent": true, "target": "#bots", "lines": 1}})
        );
    }

    #[tokio::test]
    async fn a_reply_to_a_malformed_msgid_never_reaches_irc() {
        let daemon = launch(grants());
        let reply = daemon
            .call(reply_request("#bots", "4y8k\r\nQUIT", "hi"))
            .await;
        assert_eq!(reply["Err"]["code"], "usage");
        assert!(daemon.irc.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_refused_send_never_reaches_irc_or_echoes_the_password() {
        let daemon = launch(grants());
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

    fn rate_limited_grants() -> Grants {
        toml::from_str(
            r##"
            irc.join = ["#bots", "#lounge"]
            irc.send = ["#bots", "#lounge"]
            "##,
        )
        .unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn a_targets_default_send_limit_allows_three_then_refuses_until_the_window_passes() {
        let daemon = launch(grants());
        for n in 0..3 {
            let reply = daemon
                .call(send_request("#bots", &format!("quick {n}")))
                .await;
            assert!(reply["Ok"].is_object(), "{n}: {reply:?}");
        }
        let refused = daemon.call(send_request("#bots", "one too many")).await;
        assert_eq!(refused["Err"]["code"], "rate_limited");
        assert_eq!(refused["Err"]["retry_after"], 10);

        tokio::time::advance(Duration::from_secs(10)).await;
        let reply = daemon.call(send_request("#bots", "fits now")).await;
        assert!(reply["Ok"].is_object());
    }

    #[tokio::test]
    async fn each_target_has_its_own_send_budget() {
        let limits = Limits {
            send: Limit::Window {
                count: 1,
                seconds: 10,
            },
            ..Limits::default()
        };
        let daemon = launch_with(rate_limited_grants(), limits);
        assert!(daemon.call(send_request("#bots", "hi")).await["Ok"].is_object());
        let refused = daemon.call(send_request("#bots", "again")).await;
        assert_eq!(refused["Err"]["code"], "rate_limited");
        assert!(daemon.call(send_request("#lounge", "hi")).await["Ok"].is_object());
    }

    #[tokio::test]
    async fn send_total_limits_across_every_target_even_with_room_to_spare_per_target() {
        let limits = Limits {
            send: Limit::Off,
            send_total: Limit::Window {
                count: 2,
                seconds: 10,
            },
            ..Limits::default()
        };
        let daemon = launch_with(rate_limited_grants(), limits);
        assert!(daemon.call(send_request("#bots", "one")).await["Ok"].is_object());
        assert!(daemon.call(send_request("#lounge", "two")).await["Ok"].is_object());
        let refused = daemon.call(send_request("#bots", "three")).await;
        assert_eq!(refused["Err"]["code"], "rate_limited");
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_send_does_not_count_toward_its_targets_budget() {
        let limits = Limits {
            send: Limit::Window {
                count: 1,
                seconds: 10,
            },
            ..Limits::default()
        };
        let daemon = launch_with(grants(), limits);
        assert!(daemon.call(send_request("#bots", "first")).await["Ok"].is_object());

        tokio::time::advance(Duration::from_secs(5)).await;
        let refused = daemon.call(send_request("#bots", "too soon")).await;
        assert_eq!(refused["Err"]["code"], "rate_limited");

        // If the refusal above had wrongly recorded an entry, "first"'s window would not have
        // opened up again yet.
        tokio::time::advance(Duration::from_secs(5)).await;
        let reply = daemon.call(send_request("#bots", "right on time")).await;
        assert!(reply["Ok"].is_object(), "{reply:?}");
    }

    #[tokio::test]
    async fn a_send_that_never_reaches_irc_does_not_count_against_the_budget() {
        let daemon = launch(grants());
        daemon.irc.fail_sends.store(3, Ordering::Relaxed);
        for n in 0..3 {
            let reply = daemon
                .call(send_request("#bots", &format!("retry {n}")))
                .await;
            assert_eq!(reply["Err"]["code"], "not_connected", "{n}: {reply:?}");
        }

        let reply = daemon.call(send_request("#bots", "it's back")).await;
        assert!(reply["Ok"].is_object(), "{reply:?}");
    }

    #[test]
    fn an_off_limit_never_stores_an_entry() {
        let windows = RateWindows::default();
        let now = Instant::now();
        for _ in 0..5 {
            assert_eq!(windows.reserve("#bots", Limit::Off, now), None);
        }
        assert!(windows.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn react_has_its_own_budget_independent_of_send() {
        let limits = Limits {
            react: Limit::Window {
                count: 1,
                seconds: 10,
            },
            ..Limits::default()
        };
        let daemon = launch_with(grants(), limits);
        assert!(daemon.call(react_request("#bots", "👍", false)).await["Ok"].is_object());
        let refused = daemon.call(react_request("#bots", "👍", true)).await;
        assert_eq!(refused["Err"]["code"], "rate_limited");
        assert!(daemon.call(send_request("#bots", "hi")).await["Ok"].is_object());
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_react_does_not_count_toward_its_targets_budget() {
        let limits = Limits {
            react: Limit::Window {
                count: 1,
                seconds: 10,
            },
            ..Limits::default()
        };
        let daemon = launch_with(grants(), limits);
        assert!(daemon.call(react_request("#bots", "👍", false)).await["Ok"].is_object());

        tokio::time::advance(Duration::from_secs(5)).await;
        let refused = daemon.call(react_request("#bots", "👍", true)).await;
        assert_eq!(refused["Err"]["code"], "rate_limited");

        tokio::time::advance(Duration::from_secs(5)).await;
        let reply = daemon.call(react_request("#bots", "👍", true)).await;
        assert!(reply["Ok"].is_object(), "{reply:?}");
    }

    fn react_request(target: &str, reaction: &str, remove: bool) -> Value {
        json!({"v": 1, "op": "react", "args": {
            "target": target, "msgid": "4y8kq7j2c9mnz", "reaction": reaction, "remove": remove
        }})
    }

    #[tokio::test]
    async fn react_adds_and_removes_a_reaction_where_granted_and_never_the_password() {
        let daemon = launch(grants());
        for remove in [false, true] {
            let reply = daemon.call(react_request("#bots", "👍", remove)).await;
            assert_eq!(reply["Ok"]["removed"], remove);
        }
        for (target, reaction, code) in [
            ("#lounge", "👍", "not_allowed"),
            ("#bots", PASSWORD, "blocked_secret"),
        ] {
            let refused = daemon.call(react_request(target, reaction, false)).await;
            assert_eq!(refused["Err"]["code"], code);
        }
        assert_eq!(*daemon.irc.reactions.lock().unwrap(), [false, true]);
    }

    #[tokio::test]
    async fn typing_reaches_irc_only_where_irc_typing_grants() {
        let daemon = launch(grants());
        for (target, state) in [
            ("#bots", "active"),
            ("#bots", "done"),
            ("#lounge", "active"),
        ] {
            let request =
                json!({"v": 1, "op": "typing", "args": {"target": target, "state": state}});
            let reply = daemon.call(request).await;
            if target == "#bots" {
                assert_eq!(reply["Ok"]["state"], state);
            } else {
                assert_eq!(reply["Err"]["code"], "not_allowed");
            }
        }
        assert_eq!(
            *daemon.irc.typing.lock().unwrap(),
            [Typing::Active, Typing::Done]
        );
    }

    fn redact_request(target: &str, msgid: &str, reason: &str) -> Value {
        json!({"v": 1, "op": "redact", "args": {"target": target, "msgid": msgid, "reason": reason}})
    }

    #[tokio::test]
    async fn redact_removes_only_a_message_the_bot_sent_there() {
        let daemon = launch(grants());
        let unsent = daemon
            .call(redact_request("#bots", "c2m8w1u0pq7rs", ""))
            .await;
        assert_eq!(unsent["Err"]["code"], "not_allowed");

        let sent = daemon
            .call(send_request("#bots", "wrong channel, sorry"))
            .await;
        let msgid = sent["Ok"]["msgid"].as_str().unwrap();
        for (target, msgid, code) in [
            ("#lounge", msgid, "not_allowed"),
            ("#bots", "4y8kq7j2c9mnz", "not_allowed"),
            ("#bots", msgid, "blocked_secret"),
        ] {
            let reason = if code == "blocked_secret" {
                PASSWORD
            } else {
                ""
            };
            let reply = daemon.call(redact_request(target, msgid, reason)).await;
            assert_eq!(reply["Err"]["code"], code, "{target} {msgid}");
        }
        let reply = daemon.call(redact_request("#BOTS", msgid, "oops")).await;
        assert_eq!(reply["Ok"]["msgid"], msgid);
        assert_eq!(*daemon.irc.redacted.lock().unwrap(), [msgid]);
    }

    #[test]
    fn the_sent_log_forgets_the_oldest_message_past_its_cap() {
        let mut log = SentLog::default();
        for n in 0..=SENT_LOG_CAP {
            log.record("#bots", format!("msgid{n}"));
        }
        assert!(!log.contains("#bots", "msgid0"));
        assert!(
            log.contains("#Bots", "msgid1")
                && log.contains("#bots", &format!("msgid{SENT_LOG_CAP}"))
        );
    }

    fn history_request(target: &str, limit: usize) -> Value {
        json!({"v": 1, "op": "history", "args": {"target": target, "limit": limit}})
    }

    #[tokio::test]
    async fn history_returns_sanitized_untrusted_chat_including_the_bots_own() {
        let daemon = launch(grants());
        let reply = daemon.call(history_request("#bots", 20)).await;
        assert_eq!(reply["Ok"]["notice"], NOTICE);
        assert_eq!(reply["Ok"]["complete"], true);
        assert_eq!(
            texts(&reply),
            ["louro: where is that set?", "it's in ergo.yaml"]
        );
        let events = reply["Ok"]["events"].as_array().unwrap();
        assert!(events.iter().all(|event| event["origin"] == "history"));
        assert!(events.iter().all(|event| event["untrusted"] == true));
        assert!(events.iter().all(|event| event.get("from_me").is_none()));
    }

    #[tokio::test]
    async fn history_caps_the_page_and_needs_a_read_grant() {
        let daemon = launch(grants());
        for limit in [0, 500] {
            daemon.call(history_request("#bots", limit)).await;
        }
        assert_eq!(*daemon.irc.history_limits.lock().unwrap(), [1, 100]);
        for target in ["#ops", "joao"] {
            let reply = daemon.call(history_request(target, 20)).await;
            assert_eq!(reply["Err"]["code"], "not_allowed", "{target}");
        }
        assert!(daemon.call(history_request("maria", 20)).await["Ok"].is_object());
    }

    #[tokio::test]
    async fn history_leaves_the_wait_cursor_alone() {
        let daemon = launch(grants());
        daemon.say(from_maria("louro: still there?")).await;
        daemon.call(history_request("#bots", 20)).await;
        let (_, reply) = daemon.wait(5).await;
        assert_eq!(texts(&reply), ["louro: still there?"]);
    }

    #[tokio::test]
    async fn first_lets_the_bot_dm_only_someone_who_dmed_it() {
        let daemon = launch(grants());
        let before = daemon.call(send_request("maria", "hi maria")).await;
        assert_eq!(before["Err"]["code"], "not_allowed");

        daemon.say(dm(from_maria("louro, got a sec?"))).await;
        let (_, reply) = daemon.wait(5).await;
        assert_eq!(texts(&reply), ["louro, got a sec?"]);
        let after = daemon.call(send_request("Maria", "sure, what's up?")).await;
        assert_eq!(after["Ok"]["target"], "Maria");
    }

    #[tokio::test]
    async fn first_refuses_a_send_once_a_different_account_holds_the_nick() {
        let daemon = launch(grants());
        daemon.say(dm(from_luiz("hey, got a sec?"))).await;
        daemon.wait(5).await;
        let before = daemon.call(send_request("luiz", "sure")).await;
        assert_eq!(before["Ok"]["target"], "luiz");

        let hijacked = Incoming {
            from: sender("luiz", Some("mallory")),
            ..dm(from_luiz("it's still me, luiz"))
        };
        daemon.say(hijacked).await;
        daemon.wait(5).await;

        let after = daemon.call(send_request("luiz", "sure")).await;
        assert_eq!(after["Err"]["code"], "not_allowed");
    }

    #[tokio::test]
    async fn first_still_works_once_an_accountless_sender_logs_in() {
        let daemon = launch(grants());
        let first_without_account = Incoming {
            from: sender("maria", None),
            ..dm(from_maria("hey, got a sec?"))
        };
        daemon.say(first_without_account).await;
        daemon.wait(5).await;

        daemon.say(dm(from_maria("it's me, logged in now"))).await;
        daemon.wait(5).await;

        let reply = daemon.call(send_request("maria", "sure")).await;
        assert_eq!(reply["Ok"]["target"], "maria");
    }

    fn names_request(channel: &str) -> Value {
        json!({"v": 1, "op": "names", "args": {"channel": channel}})
    }

    #[tokio::test]
    async fn names_lists_sanitized_members_of_a_joined_channel() {
        let reply = launch(grants()).call(names_request("#bots")).await;
        assert_eq!(
            reply,
            json!({"Ok": {"channel": "#bots", "members": [
                {"nick": "luiz", "prefix": "@", "account": "luiz", "away": false, "bot": false},
                {"nick": "mal[2Jlory", "prefix": "", "account": "mallory", "away": true, "bot": true},
            ]}})
        );
    }

    #[tokio::test]
    async fn names_needs_irc_join_and_the_bot_in_the_channel() {
        let daemon = launch(grants());
        for channel in ["#ops", "#lounge"] {
            let reply = daemon.call(names_request(channel)).await;
            assert_eq!(reply["Err"]["code"], "not_allowed", "{channel}");
        }
    }

    #[tokio::test]
    async fn quit_without_the_grant_is_refused_and_keeps_running() {
        let daemon = launch(grants());
        let reply = daemon.call(json!({"v": 1, "op": "quit", "args": {}})).await;
        assert_eq!(reply["Err"]["code"], "not_allowed");
        assert_eq!(daemon.status().await["Ok"]["connected"], true);
    }

    #[tokio::test]
    async fn quit_replies_then_shuts_down_and_removes_the_socket() {
        let mut grants = grants();
        grants.irc.quit = true;
        let daemon = launch(grants);
        let reply = daemon.call(json!({"v": 1, "op": "quit", "args": {}})).await;
        assert_eq!(reply, json!({"Ok": {}}));

        daemon.daemon.await.unwrap().unwrap();
        daemon.irc.quit.notified().await;
        assert!(!daemon.socket.exists());
    }

    #[tokio::test]
    async fn a_stop_signal_quits_irc_and_removes_the_socket() {
        let mut daemon = launch(grants());
        daemon.status().await;
        daemon.stop.take().unwrap().send(()).unwrap();

        daemon.daemon.await.unwrap().unwrap();
        daemon.irc.quit.notified().await;
        assert!(!daemon.socket.exists());
    }

    #[tokio::test]
    async fn louro_fails_and_removes_the_socket_once_papo_gives_up() {
        let daemon = launch(grants());
        daemon
            .update(Update::Disconnected {
                cause: "the connection was lost".into(),
                retry_in: None,
            })
            .await;
        drop(daemon.updates);

        let error = daemon.daemon.await.unwrap().unwrap_err();
        assert_eq!(
            error.to_string(),
            "the connection was lost; papo stopped reconnecting"
        );
        assert!(!daemon.socket.exists());
    }

    #[test]
    fn only_a_socket_is_removed_from_the_socket_path() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join("stale.sock");
        drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
        remove_socket(&stale).unwrap();
        assert!(fs::symlink_metadata(&stale).is_err());

        let notes = dir.path().join("notes.txt");
        fs::write(&notes, "keep me").unwrap();
        let live = dir.path().join("live.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&live).unwrap();
        let link = dir.path().join("link.sock");
        std::os::unix::fs::symlink(&live, &link).unwrap();
        for path in [&notes, &link] {
            let error = remove_socket(path).unwrap_err();
            assert_eq!(
                error.kind(),
                io::ErrorKind::AlreadyExists,
                "{}",
                path.display()
            );
        }
        assert_eq!(fs::read_to_string(&notes).unwrap(), "keep me");
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert!(fs::metadata(&live).unwrap().file_type().is_socket());
    }
}
