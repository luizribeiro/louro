//! The IRC side of the daemon: one papo client, with its chat and connection changes forwarded
//! as [`Update`]s.

use std::time::Duration;

use papo::{
    Account, Client, ConnectErrorKind, Endpoint, Event, Events, Feature, HistoryItem,
    HistoryReference, Message, MessageId, MessageKind, RegistrationError, Sasl, Source, State,
    User,
};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub use papo::Secret;

use crate::config::Server;
use crate::error::{Code, Error, Result};
use crate::text;

const UPDATES_CAPACITY: usize = 256;
const CTCP_VERSION: &str = concat!("louro ", env!("CARGO_PKG_VERSION"), " (papo)");

pub enum Update {
    Chat(Incoming),
    /// The connection dropped; `retry_in` (whole seconds) is `None` once papo has stopped
    /// reconnecting.
    Disconnected {
        cause: String,
        retry_in: Option<u64>,
    },
    /// Registration completed, first at connect and again after each reconnect.
    Registered,
    /// Someone invited the bot to `channel`; louro never joins on its own.
    Invited {
        from: Sender,
        channel: String,
    },
}

#[derive(Clone, Serialize)]
pub struct Incoming {
    #[serde(rename = "type")]
    pub kind: Kind,
    pub time: String,
    pub msgid: Option<String>,
    pub target: String,
    pub private: bool,
    pub from: Sender,
    pub text: String,
    pub mentions_me: bool,
    pub reply_to: Option<String>,
    pub origin: Origin,
    /// Sent by the bot itself; only history has these.
    #[serde(skip)]
    pub from_me: bool,
}

/// Whether chat arrived as it was sent, or from the history papo fetches after a reconnect.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Live,
    History,
}

#[derive(Clone, Serialize)]
pub struct Sender {
    pub nick: String,
    pub account: Option<String>,
    /// Marked away, as far as papo's state knows; always false for someone sharing no channel.
    pub away: bool,
    pub bot: bool,
}

impl Sender {
    /// Nicks and accounts come from other users, so they are cleaned like their chat.
    pub fn sanitized(self) -> Self {
        let clean = |name: &str| text::sanitize(name).0;
        Self {
            nick: clean(&self.nick),
            account: self.account.as_deref().map(clean),
            ..self
        }
    }
}

/// A channel member as papo's state knows it; `prefix` is the highest rank, like `@`, or empty.
#[derive(Serialize)]
pub struct Member {
    #[serde(flatten)]
    pub user: Sender,
    pub prefix: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Outgoing {
    pub target: String,
    pub kind: Kind,
    /// By the time it reaches [`Irc::send`], at most five lines joined by `\n`, with no CR, NUL
    /// or CTCP delimiter.
    pub text: String,
    /// The msgid this message answers, sent as a reply tag.
    pub reply_to: Option<String>,
}

/// Which page of a conversation's history to fetch; `before` and `after` are msgids or
/// RFC 3339 times, and with neither it is the latest `limit` messages.
#[derive(Deserialize, Serialize, clap::Args)]
pub struct HistoryQuery {
    /// How many messages to return, at most 100
    #[arg(long = "latest", default_value = "20", value_name = "N")]
    pub limit: usize,
    /// Only messages before this msgid or RFC 3339 time
    #[arg(long, value_name = "MSGID|TIME")]
    pub before: Option<String>,
    /// Only messages after this msgid or RFC 3339 time
    #[arg(long, value_name = "MSGID|TIME")]
    pub after: Option<String>,
}

pub struct History {
    pub events: Vec<Incoming>,
    /// The server has nothing older (or newer, for `after`) than this page.
    pub complete: bool,
}

pub struct Sent {
    pub msgid: Option<String>,
    /// Whether the server carries reply tags; without them a reply goes out as a plain message.
    pub threaded: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Typing {
    Active,
    Paused,
    Done,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Message,
    Notice,
    Action,
}

/// The IRC connection as the daemon sees it; `PapoIrc` below implements it over a papo client.
pub trait Irc: Send + Sync + 'static {
    fn status(&self) -> IrcStatus;
    /// The members of `channel`, sorted by nick, or `None` if the bot isn't in it.
    fn names(&self, channel: &str) -> Option<Vec<Member>>;
    /// Sends one message (several lines become a multiline batch).
    fn send(&self, message: Outgoing) -> impl Future<Output = Result<Sent>> + Send;
    /// Adds `reaction` to the message `msgid` in `target`, or removes it.
    fn react(
        &self,
        target: &str,
        msgid: &str,
        reaction: &str,
        remove: bool,
    ) -> impl Future<Output = Result<()>> + Send;
    fn typing(&self, target: &str, state: Typing) -> impl Future<Output = Result<()>> + Send;
    /// Fetches chat in `target`, a channel or the nick of a DM conversation, from the server.
    fn history(
        &self,
        target: &str,
        query: HistoryQuery,
    ) -> impl Future<Output = Result<History>> + Send;
    /// Asks the server to delete the message `msgid` in `target`; an empty reason is omitted.
    fn redact(
        &self,
        target: &str,
        msgid: &str,
        reason: &str,
    ) -> impl Future<Output = Result<()>> + Send;
    fn quit(&self) -> impl Future<Output = ()> + Send;
}

#[derive(Serialize)]
pub struct IrcStatus {
    pub connected: bool,
    pub nick: String,
    pub channels: Vec<String>,
}

pub struct PapoIrc {
    client: Client,
}

/// Connects, registers, and forwards chat from the joined channels and DMs.
pub async fn connect(
    server: &Server,
    password: Option<Secret>,
) -> Result<(PapoIrc, mpsc::Receiver<Update>)> {
    let mut builder = Client::builder(endpoint(&server.address)?)
        .nick(&server.nick)
        .alt_nicks(server.alt_nicks.clone())
        .bot()
        .ctcp_version(Some(CTCP_VERSION))
        .ctcp_source(env!("CARGO_PKG_REPOSITORY"))
        .autojoin(server.autojoin.clone())
        .history_on_reconnect(true);
    if let Some(username) = &server.username {
        builder = builder.username(username);
    }
    if let Some(realname) = &server.realname {
        builder = builder.realname(realname);
    }
    if let (Some(user), Some(password)) = (server.sasl_username(), password) {
        builder = builder.sasl(Sasl::any(user, password));
    }
    let (client, events) = builder
        .connect()
        .await
        .map_err(|error| Error::new(connect_code(&error), error.to_string()))?;
    let (sender, updates) = mpsc::channel(UPDATES_CAPACITY);
    tokio::spawn(forward(client.clone(), events, sender));
    Ok((PapoIrc { client }, updates))
}

/// papo's builder takes a bare host or an `Endpoint`, but has no public parser for `irc://` and
/// `ircs://` URLs, and papo deliberately won't parse IRC URLs at all: a typo'd scheme could
/// silently downgrade to plaintext, and credentials can hide in userinfo.
fn endpoint(address: &str) -> Result<Endpoint> {
    const USAGE: &str =
        "use a host, `irc://host[:port]`, `ircs://host[:port]`, `ws://...` or `wss://...`";
    let (scheme, rest) = address.split_once("://").unwrap_or(("ircs", address));
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "irc" | "ircs" | "ws" | "wss") {
        return Err(
            Error::new(Code::Usage, format!("unsupported scheme {scheme:?}")).with_hint(USAGE),
        );
    }
    if rest.contains('@') {
        return Err(
            Error::new(Code::Usage, "server address can't carry credentials")
                .with_hint("use sasl_username and password_file instead"),
        );
    }
    if scheme == "ws" || scheme == "wss" {
        return Ok(Endpoint::websocket(address));
    }
    let (host_port, path) = rest.split_once('/').unwrap_or((rest, ""));
    if !path.is_empty() {
        return Err(
            Error::new(Code::Usage, "server address can't contain a path").with_hint(USAGE),
        );
    }
    let (tls, default_port) = if scheme == "ircs" {
        (true, 6697)
    } else {
        (false, 6667)
    };
    let (host, port) = host_port
        .rsplit_once(':')
        .and_then(|(host, port)| Some((host, port.parse().ok()?)))
        .unwrap_or((host_port, default_port));
    let host = host.trim_matches(['[', ']']);
    Ok(if tls {
        Endpoint::tls(host, port)
    } else {
        Endpoint::plaintext(host, port)
    })
}

fn connect_code(error: &papo::Error) -> Code {
    match error {
        papo::Error::Registration(
            RegistrationError::Sasl { .. }
            | RegistrationError::SaslUnavailable
            | RegistrationError::PasswordIncorrect
            | RegistrationError::AccountRequired,
        ) => Code::AuthFailed,
        papo::Error::Connect(failure)
            if matches!(
                failure.kind(),
                ConnectErrorKind::Tls | ConnectErrorKind::Certificate | ConnectErrorKind::TlsConfig
            ) =>
        {
            Code::Tls
        }
        _ => Code::ConnectFailed,
    }
}

async fn forward(client: Client, mut events: Events, updates: mpsc::Sender<Update>) {
    while let Some(event) = events.next().await {
        for update in to_updates(event, &client.state()) {
            if updates.send(update).await.is_err() {
                return;
            }
        }
    }
}

fn to_updates(event: Event, state: &State) -> Vec<Update> {
    match event {
        Event::Message(message) => to_incoming(&message, state)
            .map(Update::Chat)
            .into_iter()
            .collect(),
        Event::History(page) => page
            .items
            .iter()
            .filter_map(HistoryItem::message)
            .filter_map(|message| to_incoming(message, state))
            .map(Update::Chat)
            .collect(),
        Event::Registered(_) => vec![Update::Registered],
        Event::Invited(invited) if invited.is_self => invited
            .by
            .nick()
            .map(|nick| Update::Invited {
                from: known_sender(state, nick),
                channel: invited.channel.to_string(),
            })
            .into_iter()
            .collect(),
        Event::Disconnected(disconnected) => vec![Update::Disconnected {
            cause: disconnected.cause.to_string(),
            retry_in: disconnected.retry_in.map(whole_seconds),
        }],
        _ => Vec::new(),
    }
}

/// A user as papo's state knows them, for events that carry no account tag of their own.
fn known_sender(state: &State, nick: &str) -> Sender {
    let user = state.user(nick);
    Sender {
        nick: nick.to_owned(),
        account: user.and_then(|user| logged_in(user.account())),
        away: is_away(state, nick),
        bot: user.is_some_and(User::is_bot),
    }
}

fn is_away(state: &State, nick: &str) -> bool {
    state.user(nick).is_some_and(|user| user.away().is_some())
}

fn logged_in(account: &Account) -> Option<String> {
    match account {
        Account::LoggedIn(account) => Some(account.to_string()),
        _ => None,
    }
}

/// Rounds `delay` up to whole seconds, for a `retry_after` or `retry_in` an agent can poll on.
pub(crate) fn whole_seconds(delay: Duration) -> u64 {
    delay.as_secs() + u64::from(delay.subsec_nanos() > 0)
}

fn to_incoming(message: &Message, state: &State) -> Option<Incoming> {
    let private = state.is_me(message.target());
    if !private && state.channel(message.target()).is_none() {
        return None;
    }
    let incoming = chat(message, state, message.reply_target(), private)?;
    let origin = chat_origin(message.origin(), incoming.from_me)?;
    Some(Incoming { origin, ..incoming })
}

fn chat(message: &Message, state: &State, target: &str, private: bool) -> Option<Incoming> {
    let kind = chat_kind(message.kind())?;
    let nick = message.sender.as_ref().and_then(Source::nick)?;
    let from_me = state.is_me(nick)
        || message
            .account()
            .zip(state.account())
            .is_some_and(|(sender, me)| state.casemapping().eq(sender, me));
    Some(Incoming {
        kind,
        time: humantime::format_rfc3339_millis(message.time()).to_string(),
        msgid: message.id().map(ToString::to_string),
        target: target.to_owned(),
        private,
        from: Sender {
            nick: nick.to_owned(),
            account: message.account().map(str::to_owned),
            away: is_away(state, nick),
            bot: message.is_bot(),
        },
        text: message.text().to_owned(),
        mentions_me: private
            || message
                .text()
                .to_lowercase()
                .contains(&state.nick().to_lowercase()),
        reply_to: message.reply_to().map(str::to_owned),
        origin: Origin::History,
        from_me,
    })
}

/// An RFC 3339 time, else a msgid.
fn history_reference(raw: &str) -> Result<HistoryReference> {
    if let Ok(time) = humantime::parse_rfc3339_weak(raw) {
        return Ok(time.into());
    }
    text::check_msgid(raw)?;
    Ok(raw.into())
}

/// papo marks our own echoes and other sessions only on live messages, so history from this
/// account is recognized by `from_me`. Replayed batches (e.g. bouncer playback) are dropped.
fn chat_origin(origin: papo::Origin, from_me: bool) -> Option<Origin> {
    match origin {
        papo::Origin::Live => Some(Origin::Live),
        papo::Origin::History if !from_me => Some(Origin::History),
        _ => None,
    }
}

fn chat_kind(kind: MessageKind) -> Option<Kind> {
    match kind {
        MessageKind::Privmsg => Some(Kind::Message),
        MessageKind::Notice => Some(Kind::Notice),
        MessageKind::Action => Some(Kind::Action),
        _ => None,
    }
}

impl Irc for PapoIrc {
    fn status(&self) -> IrcStatus {
        let state = self.client.state();
        IrcStatus {
            connected: state.is_connected(),
            nick: state.nick().to_string(),
            channels: state
                .channels()
                .map(|channel| channel.name().to_string())
                .collect(),
        }
    }

    fn names(&self, channel: &str) -> Option<Vec<Member>> {
        let state = self.client.state();
        let mut members: Vec<Member> = state
            .channel(channel)?
            .members()
            .map(|member| Member {
                user: known_sender(&state, member.nick()),
                prefix: member
                    .highest_prefix()
                    .map(String::from)
                    .unwrap_or_default(),
            })
            .collect();
        members.sort_by(|a, b| a.user.nick.cmp(&b.user.nick));
        Some(members)
    }

    async fn send(&self, message: Outgoing) -> Result<Sent> {
        let Outgoing {
            target,
            kind,
            text,
            reply_to,
        } = message;
        let mut request = match kind {
            Kind::Message => self.client.privmsg(&target, &text),
            Kind::Notice => self.client.notice(&target, &text),
            Kind::Action => self.client.action(&target, flatten_for_action(&text)),
        };
        if let Some(id) = &reply_to {
            request = request.reply_to(&MessageId::from(id));
        }
        let threaded = reply_to.is_some() && self.client.state().supports(Feature::Replies);
        let sent = request.await?;
        Ok(Sent {
            msgid: sent.id.map(|id| id.to_string()),
            threaded,
        })
    }

    async fn react(&self, target: &str, msgid: &str, reaction: &str, remove: bool) -> Result<()> {
        let request = if remove {
            self.client.unreact(target, msgid, reaction)
        } else {
            self.client.react(target, msgid, reaction)
        };
        Ok(request.await?)
    }

    async fn typing(&self, target: &str, state: Typing) -> Result<()> {
        let state = match state {
            Typing::Active => papo::Typing::Active,
            Typing::Paused => papo::Typing::Paused,
            Typing::Done => papo::Typing::Done,
        };
        Ok(self.client.typing(target, state).await?)
    }

    async fn history(&self, target: &str, query: HistoryQuery) -> Result<History> {
        let before = query.before.as_deref().map(history_reference).transpose()?;
        let after = query.after.as_deref().map(history_reference).transpose()?;
        let history = self.client.history(target);
        let request = match (after, before) {
            (None, None) => history.latest(query.limit),
            (Some(after), None) => history.after(after),
            (None, Some(before)) => history.before(before),
            (Some(after), Some(before)) => history.between(after, before),
        };
        let page = request.limit(query.limit).await?;
        let state = self.client.state();
        Ok(History {
            // Filed under the conversation asked for, which the bot's own lines are part of.
            events: page
                .messages()
                .filter_map(|message| chat(message, &state, target, message.is_private()))
                .collect(),
            complete: page.end,
        })
    }

    async fn redact(&self, target: &str, msgid: &str, reason: &str) -> Result<()> {
        Ok(self.client.redact(target, msgid, reason).await?)
    }

    async fn quit(&self) {
        let _ = self.client.quit("").await;
    }
}

/// papo's CTCP builder panics on a raw newline, so an action's lines are joined onto one.
fn flatten_for_action(text: &str) -> String {
    text.replace('\n', " ")
}

impl From<papo::Error> for Error {
    fn from(error: papo::Error) -> Self {
        let code = match error {
            papo::Error::Disconnected(_) => Code::NotConnected,
            papo::Error::Timeout => Code::RequestTimeout,
            papo::Error::Server(_) => Code::ServerError,
            papo::Error::InvalidInput(_) => Code::Usage,
            papo::Error::Unsupported(_) => Code::Unsupported,
            _ => Code::Internal,
        };
        Self::new(code, error.to_string())
    }
}

#[cfg(test)]
/// Chat from the people the unit tests use.
pub mod fixtures {
    use super::{Incoming, Kind, Origin, Sender};

    pub fn sender(nick: &str, account: Option<&str>) -> Sender {
        Sender {
            nick: nick.into(),
            account: account.map(String::from),
            away: false,
            bot: false,
        }
    }

    pub fn from_maria(text: &str) -> Incoming {
        Incoming {
            kind: Kind::Message,
            time: "2026-10-16T21:04:11.392Z".into(),
            msgid: Some("4y8kq7j2c9mnz".into()),
            target: "#bots".into(),
            private: false,
            from: sender("maria", Some("maria")),
            text: text.into(),
            mentions_me: true,
            reply_to: None,
            origin: Origin::Live,
            from_me: false,
        }
    }

    pub fn from_joao(text: &str) -> Incoming {
        Incoming {
            msgid: Some("9qq1v0x3hdk2a".into()),
            from: sender("joao", None),
            mentions_me: false,
            reply_to: Some("4y8kq7j2c9mnz".into()),
            ..from_maria(text)
        }
    }

    pub fn from_luiz(text: &str) -> Incoming {
        Incoming {
            from: sender("luiz", Some("luiz")),
            ..from_maria(text)
        }
    }

    pub fn dm(incoming: Incoming) -> Incoming {
        Incoming {
            private: true,
            target: incoming.from.nick.clone(),
            ..incoming
        }
    }
}

#[cfg(test)]
mod tests {
    use papo::{DisconnectCause, Disconnected, InvalidInput, Unsupported, UnsupportedReason};

    use super::*;

    #[test]
    fn forwards_live_chat_and_others_history_only() {
        for (origin, from_me, forwarded) in [
            (papo::Origin::Live, false, Some(Origin::Live)),
            (papo::Origin::History, false, Some(Origin::History)),
            (papo::Origin::History, true, None),
            (papo::Origin::Replay, false, None),
            (papo::Origin::Echo, true, None),
            (papo::Origin::OtherSession, true, None),
        ] {
            assert_eq!(chat_origin(origin, from_me), forwarded, "{origin:?}");
        }
    }

    #[test]
    fn a_history_reference_is_a_time_or_a_msgid() {
        for raw in [
            "2026-10-16T21:04:11Z",
            "2026-10-16T21:04:11.392Z",
            "4y8kq7j2c9mnz",
        ] {
            assert!(history_reference(raw).is_ok(), "{raw}");
        }
        let error = history_reference("4y8k q7j2").err().unwrap();
        assert_eq!(error.code, Code::Usage);
    }

    #[test]
    fn a_retry_delay_rounds_up_to_whole_seconds() {
        for (delay, seconds) in [
            (Duration::ZERO, 0),
            (Duration::from_millis(1), 1),
            (Duration::from_secs(1), 1),
            (Duration::from_millis(1_500), 2),
            (Duration::from_secs(300), 300),
        ] {
            assert_eq!(whole_seconds(delay), seconds, "{delay:?}");
        }
    }

    #[test]
    fn an_action_is_one_line() {
        assert_eq!(
            flatten_for_action("checks the logs\nfinds nothing"),
            "checks the logs finds nothing"
        );
    }

    #[test]
    fn addresses_are_hosts_or_irc_urls() {
        for (address, expected) in [
            (
                "irc.weblounge.org",
                Endpoint::tls("irc.weblounge.org", 6697),
            ),
            (
                "ircs://irc.weblounge.org",
                Endpoint::tls("irc.weblounge.org", 6697),
            ),
            (
                "IRCS://irc.weblounge.org:7000",
                Endpoint::tls("irc.weblounge.org", 7000),
            ),
            (
                "irc://127.0.0.1:6667",
                Endpoint::plaintext("127.0.0.1", 6667),
            ),
            ("irc://localhost", Endpoint::plaintext("localhost", 6667)),
            ("irc://[::1]:6667", Endpoint::plaintext("::1", 6667)),
            ("ircs://[::1]", Endpoint::tls("::1", 6697)),
            (
                "IRC://irc.weblounge.org",
                Endpoint::plaintext("irc.weblounge.org", 6667),
            ),
            (
                "ircs://irc.weblounge.org:6697/",
                Endpoint::tls("irc.weblounge.org", 6697),
            ),
            (
                "ircs://[2001:db8::1]:6697",
                Endpoint::tls("2001:db8::1", 6697),
            ),
            (
                "wss://irc.weblounge.org/webirc",
                Endpoint::websocket("wss://irc.weblounge.org/webirc"),
            ),
        ] {
            assert_eq!(endpoint(address).unwrap(), expected, "{address}");
        }
    }

    #[test]
    fn an_unknown_scheme_or_a_path_is_refused() {
        for address in [
            "irs://irc.weblounge.org",
            "ircs://irc.weblounge.org/channel",
        ] {
            assert_eq!(
                endpoint(address).unwrap_err().code,
                Code::Usage,
                "{address}"
            );
        }
    }

    #[test]
    fn userinfo_is_refused_without_echoing_the_secret() {
        for address in [
            "irc://louro:secret@irc.weblounge.org",
            "wss://louro:secret@irc.weblounge.org/webirc",
        ] {
            let error = endpoint(address).unwrap_err();
            assert_eq!(error.code, Code::Usage, "{address}");
            assert!(!format!("{error}").contains("secret"), "{address}: {error}");
            let hint = error.hint.as_ref().unwrap();
            assert!(
                hint.contains("sasl_username") && hint.contains("password_file"),
                "{address}"
            );
        }
    }

    #[test]
    fn papo_errors_map_to_louro_codes() {
        let lost = Disconnected::new(DisconnectCause::ConnectionLost, None);
        for (error, code) in [
            (papo::Error::Disconnected(lost), Code::NotConnected),
            (papo::Error::Timeout, Code::RequestTimeout),
            (
                papo::Error::InvalidInput(InvalidInput::EmptyText),
                Code::Usage,
            ),
            (
                papo::Error::Unsupported(Unsupported::new(
                    Feature::Reactions,
                    UnsupportedReason::NotOffered,
                )),
                Code::Unsupported,
            ),
        ] {
            assert_eq!(Error::from(error).code, code);
        }
        let refused = papo::Error::Registration(RegistrationError::SaslUnavailable);
        assert_eq!(connect_code(&refused), Code::AuthFailed);
    }

    #[test]
    fn keeps_chat_and_drops_other_ctcp() {
        for (kind, chat) in [
            (MessageKind::Privmsg, Some(Kind::Message)),
            (MessageKind::Notice, Some(Kind::Notice)),
            (MessageKind::Action, Some(Kind::Action)),
            (MessageKind::Ctcp, None),
        ] {
            assert_eq!(chat_kind(kind), chat);
        }
    }
}
