//! Deny-by-default checks of what the agent asks for against louro.toml's `[grants]`.

use crate::config::{Grants, Selector};
use crate::error::{Code, Error, Result};
use crate::irc::{Incoming, Outgoing, Sender};
use crate::socket::Op;

/// In `irc.dm.send`: anyone whose DM the daemon has received.
const FIRST: &str = "$first";

/// What the daemon knows about DMs received from a target nick, for `irc.dm.send`'s `$first`
/// and `account:` entries. Both come from the same nick→account bookkeeping (see `daemon.rs`).
#[derive(Clone, Default)]
pub struct DmHistory {
    /// The nick has sent the bot a DM and, if that DM carried an account, every DM from it since
    /// has carried the same one: `$first` may still name it.
    pub first: bool,
    /// The nick's account, known only while every DM from it has carried the same one.
    pub account: Option<String>,
}

impl DmHistory {
    /// What still holds for whoever has the nick now, logged in to `account`: `$first` only if
    /// the DMs recorded an account and it is theirs, or recorded none, and `account:` entries
    /// only for their own account.
    pub fn held_by(&self, account: Option<&str>) -> Self {
        let same_account = self.account.as_deref().is_none_or(|recorded| {
            account.is_some_and(|account| account.eq_ignore_ascii_case(recorded))
        });
        Self {
            first: self.first && same_account,
            account: account.map(str::to_ascii_lowercase),
        }
    }
}

impl Grants {
    /// Channel chat only from `irc.join` channels, DMs only from `irc.dm.read` senders.
    pub fn may_read(&self, incoming: &Incoming) -> bool {
        if incoming.private {
            self.may_hear_privately_from(&incoming.from)
        } else {
            self.irc.join.matches(&incoming.target)
        }
    }

    /// Private contact, a DM or an invite, reaches the agent only from `irc.dm.read` senders.
    pub fn may_hear_privately_from(&self, from: &Sender) -> bool {
        self.irc
            .dm
            .read
            .matches_sender(&from.nick, from.account.as_deref())
    }

    /// Whether the agent may make the request `op`; `dm` is what DMs from its target recorded.
    pub fn check(&self, op: &Op, dm: &DmHistory) -> Result<()> {
        let irc = &self.irc;
        match op {
            Op::Status {} | Op::Wait { .. } => Ok(()),
            Op::Send(Outgoing { target, .. }) => self.check_send(target, dm),
            Op::React { target, .. } => self.check_also(&irc.react, "irc.react", target, dm),
            Op::Typing { target, .. } => self.check_also(&irc.typing, "irc.typing", target, dm),
            Op::History { target, .. } => match target_kind(target)? {
                TargetKind::Channel => self.check_join(target),
                TargetKind::Nick => require(
                    irc.dm.read.matches_sender(target, dm.account.as_deref()),
                    "irc.dm.read",
                    target,
                ),
            },
            Op::Names { channel }
            | Op::Join { channel }
            | Op::Part { channel, .. }
            | Op::Topic { channel, set: None } => self.check_join(channel),
            Op::Topic { channel, .. } => {
                self.check_join(channel)?;
                require(irc.topic.matches(channel), "irc.topic", channel)
            }
            Op::Watch { nick, .. } => {
                expect_kind(nick, TargetKind::Nick)?;
                require(irc.watch.matches(nick), "irc.watch", nick)
            }
            Op::Whois { nick } | Op::Whowas { nick } => {
                granted(irc.whois, "irc.whois")?;
                expect_kind(nick, TargetKind::Nick)
            }
            Op::Redact { .. } => granted(irc.redact, "irc.redact"),
            Op::List { .. } => granted(irc.list, "irc.list"),
            Op::Nick { .. } | Op::Away { .. } | Op::Setname { .. } => {
                granted(irc.identity, "irc.identity")
            }
            Op::Quit {} if irc.quit => Ok(()),
            Op::Quit {} => Err(Error::new(Code::NotAllowed, "irc.quit is not granted")
                .with_hint("the operator stops louro")),
        }
    }

    /// A channel needs `irc.join` and `irc.send`; a nick needs `irc.dm.send`.
    fn check_send(&self, target: &str, dm: &DmHistory) -> Result<()> {
        match target_kind(target)? {
            TargetKind::Channel => require(
                self.irc.join.matches(target) && self.irc.send.matches(target),
                "irc.send",
                target,
            ),
            TargetKind::Nick => {
                let dm_send = &self.irc.dm.send;
                let allowed = dm_send.matches_sender(target, dm.account.as_deref())
                    || dm.first && dm_send.matches(FIRST);
                require(allowed, "irc.dm.send", target)
            }
        }
    }

    /// A grant that narrows sending, like `irc.react`: the target needs both.
    fn check_also(&self, grant: &Selector, name: &str, target: &str, dm: &DmHistory) -> Result<()> {
        self.check_send(target, dm)?;
        require(
            grant.matches_sender(target, dm.account.as_deref()),
            name,
            target,
        )
    }

    /// Joining a channel, or reading what it holds, like its members, needs `irc.join`.
    fn check_join(&self, channel: &str) -> Result<()> {
        expect_kind(channel, TargetKind::Channel)?;
        require(self.irc.join.matches(channel), "irc.join", channel)
    }
}

fn granted(granted: bool, grant: &str) -> Result<()> {
    if granted {
        return Ok(());
    }
    Err(not_allowed(format!("{grant} is not granted")))
}

fn require(granted: bool, grant: &str, target: &str) -> Result<()> {
    if granted {
        return Ok(());
    }
    Err(not_allowed(format!("{grant} does not grant {target}")))
}

#[derive(Clone, Copy, PartialEq)]
enum TargetKind {
    Channel,
    Nick,
}

/// Refuses anything that isn't one channel or one nick.
fn target_kind(target: &str) -> Result<TargetKind> {
    // `PRIVMSG maria,#lounge` would also reach a target no grant covers.
    if target.contains(',') {
        return Err(not_allowed("name one target at a time"));
    }
    if target.starts_with(['#', '&', '+', '!']) {
        return Ok(TargetKind::Channel);
    }
    if target.starts_with(['@', '%', '~']) {
        return Err(not_allowed(format!(
            "{target} is a status message to a channel, not a nick"
        )));
    }
    // `$` starts a server mask, and keeps `$first` from naming a nick. Reject whitespace and
    // control characters here rather than relying on papo: its own checks differ by operation
    // and none of them reject a stray control character outside `\0`, `\r`, `\n`, and space.
    if target.is_empty()
        || target.starts_with([':', '$'])
        || target.contains(|c: char| c.is_whitespace() || c.is_control())
    {
        return Err(not_allowed(format!(
            "{target} is not a valid message target"
        )));
    }
    Ok(TargetKind::Nick)
}

fn expect_kind(target: &str, kind: TargetKind) -> Result<()> {
    if target_kind(target)? == kind {
        return Ok(());
    }
    let noun = match kind {
        TargetKind::Channel => "a channel",
        TargetKind::Nick => "a nick",
    };
    Err(not_allowed(format!("{target} is not {noun}")))
}

fn not_allowed(message: impl Into<String>) -> Error {
    Error::new(Code::NotAllowed, message).with_hint("don't retry; the operator chose this")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::irc::HistoryQuery;
    use crate::irc::fixtures::{from_maria, sender};

    fn grants(toml: &str) -> Grants {
        toml::from_str(toml).unwrap()
    }

    fn bots_and_dms() -> Grants {
        grants(
            r##"
            irc.join = ["#bots", "#lounge"]
            irc.send = ["#bots"]
            irc.dm.read = ["luiz"]
            irc.dm.send = true
            "##,
        )
    }

    fn incoming(target: &str, nick: &str, private: bool) -> Incoming {
        Incoming {
            target: target.into(),
            private,
            from: sender(nick, None),
            ..from_maria("louro: ping")
        }
    }

    fn check(grants: &Grants, op: &Op) -> Result<()> {
        grants.check(op, &DmHistory::default())
    }

    fn history(target: &str) -> Op {
        Op::History {
            target: target.into(),
            query: HistoryQuery {
                limit: 20,
                before: None,
                after: None,
            },
        }
    }

    fn send(grants: &Grants, target: &str, dm_received: bool) -> Result<()> {
        let dm = DmHistory {
            first: dm_received,
            account: None,
        };
        grants.check_send(target, &dm)
    }

    #[test]
    fn first_allows_dms_only_to_people_who_dmed_the_bot() {
        let grants = grants(r#"irc.dm.send = ["$first", "luiz"]"#);
        assert_eq!(
            send(&grants, "maria", false).unwrap_err().code,
            Code::NotAllowed
        );
        assert!(send(&grants, "maria", true).is_ok());
        assert!(send(&grants, "luiz", false).is_ok());
        assert!(send(&grants, "$first", true).is_err());
    }

    #[test]
    fn first_holds_only_for_the_account_that_dmed_the_bot() {
        let luiz = DmHistory {
            first: true,
            account: Some("luiz".into()),
        };
        let accountless = DmHistory {
            first: true,
            account: None,
        };
        for (dm, now, first) in [
            (&luiz, Some("Luiz"), true),
            (&luiz, Some("mallory"), false),
            (&luiz, None, false),
            (&accountless, Some("mallory"), true),
            (&accountless, None, true),
        ] {
            let held = dm.held_by(now);
            assert_eq!(held.first, first, "{:?} now {now:?}", dm.account);
            assert_eq!(held.account, now.map(str::to_ascii_lowercase));
        }
    }

    #[test]
    fn reads_only_joined_channels_and_dms_from_dm_read() {
        let grants = bots_and_dms();
        for (target, nick, private, readable) in [
            ("#bots", "maria", false, true),
            ("#Lounge", "maria", false, true),
            ("#ops", "maria", false, false),
            ("Luiz", "Luiz", true, true),
            ("maria", "maria", true, false),
        ] {
            let event = incoming(target, nick, private);
            assert_eq!(grants.may_read(&event), readable, "{target} from {nick}");
        }
    }

    #[test]
    fn sends_only_where_irc_send_and_irc_dm_send_grant() {
        let grants = bots_and_dms();
        for target in ["#bots", "#BOTS", "maria"] {
            assert!(send(&grants, target, false).is_ok());
        }
        for target in [
            "#lounge",
            "#ops",
            "&ops",
            "+ops",
            "!ABCDEops",
            "maria,#ops",
            "#bots,#ops",
            "@#lounge",
            "+#bots",
            "~#lounge",
            "%#lounge",
            ":evil",
            "$*.weblounge.org",
            "#bots extra",
            "",
            "maria\r\nQUIT",
            "maria\u{7}",
        ] {
            let error = send(&grants, target, true).unwrap_err();
            assert_eq!(error.code, Code::NotAllowed, "{target}");
        }
    }

    #[test]
    fn dm_send_accepts_account_selectors_for_the_senders_known_account() {
        let grants = grants(r#"irc.dm.send = ["account:luiz"]"#);
        for (account, allowed) in [
            (Some("luiz"), true),
            (Some("mallory"), false),
            (None, false),
        ] {
            let dm = DmHistory {
                first: false,
                account: account.map(String::from),
            };
            assert_eq!(grants.check_send("lz", &dm).is_ok(), allowed, "{account:?}");
        }
    }

    #[test]
    fn reacts_only_where_irc_react_and_sending_both_grant() {
        let grants = grants(
            r##"
            irc.join = ["#bots", "#lounge"]
            irc.send = ["#bots", "#lounge"]
            irc.dm.send = ["luiz", "maria"]
            irc.react = ["#bots", "account:luiz", "joao"]
            "##,
        );
        let react = |target: &str, account: Option<&str>| {
            let dm = DmHistory {
                first: false,
                account: account.map(String::from),
            };
            grants.check_also(&grants.irc.react, "irc.react", target, &dm)
        };
        assert!(react("#bots", None).is_ok());
        assert!(react("luiz", Some("luiz")).is_ok());
        for (target, account) in [
            ("#lounge", None),
            ("maria", None),
            ("joao", None),
            ("#ops", None),
        ] {
            assert_eq!(
                react(target, account).unwrap_err().code,
                Code::NotAllowed,
                "{target}"
            );
        }
    }

    #[test]
    fn reads_history_of_joined_channels_and_dm_read_conversations() {
        let grants = grants(
            r##"
            irc.join = ["#bots"]
            irc.dm.read = ["maria", "account:luiz"]
            "##,
        );
        let luiz = DmHistory {
            first: true,
            account: Some("luiz".into()),
        };
        let unknown = DmHistory::default();
        for (target, dm) in [("#BOTS", &unknown), ("maria", &unknown), ("lz", &luiz)] {
            assert!(grants.check(&history(target), dm).is_ok(), "{target}");
        }
        for (target, dm) in [
            ("#lounge", &unknown),
            ("lz", &unknown),
            ("#bots,#ops", &unknown),
        ] {
            let error = grants.check(&history(target), dm).unwrap_err();
            assert_eq!(error.code, Code::NotAllowed, "{target}");
        }
    }

    #[test]
    fn sets_a_topic_only_where_irc_join_and_irc_topic_both_grant() {
        let grants = grants(
            r##"
            irc.join = ["#bots", "#lounge"]
            irc.topic = ["#bots", "#ops"]
            "##,
        );
        let topic = |channel: &str| Op::Topic {
            channel: channel.into(),
            set: Some("rust help".into()),
        };
        assert!(check(&grants, &topic("#BOTS")).is_ok());
        for channel in ["#lounge", "#ops"] {
            let error = check(&grants, &topic(channel)).unwrap_err();
            assert_eq!(error.code, Code::NotAllowed, "{channel}");
        }
    }

    #[test]
    fn joins_only_a_channel_that_irc_join_grants() {
        let bots_and_help = grants(r##"irc.join = ["#bots", "#help-*"]"##);
        for channel in ["#bots", "#help-rust"] {
            assert!(bots_and_help.check_join(channel).is_ok(), "{channel}");
        }
        let error = bots_and_help.check_join("#lounge").unwrap_err();
        assert_eq!(error.message, "irc.join does not grant #lounge");
        let everywhere = grants("irc.join = true");
        let error = everywhere.check_join("maria").unwrap_err();
        assert_eq!(error.message, "maria is not a channel");
    }

    #[test]
    fn watches_only_nicks_that_irc_watch_grants() {
        let grants = grants(r#"irc.watch = ["luiz", "maria*"]"#);
        let watch = |nick: &str| Op::Watch {
            nick: nick.into(),
            remove: false,
        };
        for nick in ["Luiz", "maria_"] {
            assert!(check(&grants, &watch(nick)).is_ok(), "{nick}");
        }
        for target in ["mallory", "#bots", "luiz,mallory"] {
            let error = check(&grants, &watch(target)).unwrap_err();
            assert_eq!(error.code, Code::NotAllowed, "{target}");
        }
    }

    #[test]
    fn looks_up_only_a_nick_and_only_when_irc_whois_grants() {
        let whois = |nick: &str| Op::Whois { nick: nick.into() };
        let error = check(&Grants::default(), &whois("luiz")).unwrap_err();
        assert_eq!(error.message, "irc.whois is not granted");
        let grants = grants("irc.whois = true");
        assert!(check(&grants, &whois("luiz")).is_ok());
        for target in ["#bots", "luiz,maria", "luiz\r\nQUIT", ""] {
            let error = check(&grants, &whois(target)).unwrap_err();
            assert_eq!(error.code, Code::NotAllowed, "{target:?}");
        }
    }

    #[test]
    fn redacts_lists_and_quits_only_when_granted() {
        let redact = Op::Redact {
            target: "#bots".into(),
            msgid: "c2m8w1u0pq7rs".into(),
            reason: String::new(),
        };
        let list = Op::List { pattern: None };
        let nick = Op::Nick {
            nick: "louro_".into(),
        };
        for (grant, op) in [
            ("irc.redact", &redact),
            ("irc.list", &list),
            ("irc.identity", &nick),
            ("irc.quit", &Op::Quit {}),
        ] {
            let error = check(&Grants::default(), op).unwrap_err();
            assert_eq!(error.code, Code::NotAllowed, "{grant}");
            assert!(check(&grants(&format!("{grant} = true")), op).is_ok());
        }
        let error = check(&Grants::default(), &Op::Quit {}).unwrap_err();
        assert_eq!(error.hint.as_deref(), Some("the operator stops louro"));
    }

    #[test]
    fn grants_nothing_by_default() {
        let none = Grants::default();
        assert!(!none.may_read(&incoming("#bots", "maria", false)));
        assert!(!none.may_read(&incoming("luiz", "luiz", true)));
        for target in ["#bots", "luiz"] {
            assert!(send(&none, target, true).is_err());
        }
    }
}
