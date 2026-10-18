//! Deny-by-default checks of what the agent asks for against louro.toml's `[grants]`.

use crate::config::{Grants, Selector};
use crate::error::{Code, Error, Result};
use crate::irc::Incoming;

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

impl Grants {
    /// Channel chat only from `irc.join` channels, DMs only from `irc.dm.read` senders.
    pub fn may_read(&self, incoming: &Incoming) -> bool {
        if incoming.private {
            self.irc
                .dm
                .read
                .matches_sender(&incoming.from.nick, incoming.from.account.as_deref())
        } else {
            self.irc.join.matches(&incoming.target)
        }
    }

    /// A channel needs `irc.join` and `irc.send`; a nick needs `irc.dm.send`.
    pub fn check_send(&self, target: &str, dm: &DmHistory) -> Result<()> {
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
    pub fn check_also(
        &self,
        grant: &Selector,
        name: &str,
        target: &str,
        dm: &DmHistory,
    ) -> Result<()> {
        self.check_send(target, dm)?;
        require(
            grant.matches_sender(target, dm.account.as_deref()),
            name,
            target,
        )
    }

    /// A channel's history needs `irc.join`; a DM conversation's needs `irc.dm.read` for the nick.
    pub fn check_history(&self, target: &str, dm: &DmHistory) -> Result<()> {
        match target_kind(target)? {
            TargetKind::Channel => self.check_join(target),
            TargetKind::Nick => require(
                self.irc
                    .dm
                    .read
                    .matches_sender(target, dm.account.as_deref()),
                "irc.dm.read",
                target,
            ),
        }
    }

    /// Reading what a channel holds, like its members, needs `irc.join`.
    pub fn check_join(&self, channel: &str) -> Result<()> {
        require(self.irc.join.matches(channel), "irc.join", channel)
    }

    /// Setting a channel's topic needs `irc.topic` on top of `irc.join`.
    pub fn check_topic(&self, channel: &str) -> Result<()> {
        self.check_join(channel)?;
        require(self.irc.topic.matches(channel), "irc.topic", channel)
    }

    pub fn check_redact(&self) -> Result<()> {
        granted(self.irc.redact, "irc.redact")
    }

    /// `whois` and `whowas` look up one nick.
    pub fn check_whois(&self, nick: &str) -> Result<()> {
        granted(self.irc.whois, "irc.whois")?;
        match target_kind(nick)? {
            TargetKind::Nick => Ok(()),
            TargetKind::Channel => Err(not_allowed(format!("{nick} is a channel, not a nick"))),
        }
    }

    pub fn check_quit(&self) -> Result<()> {
        if self.irc.quit {
            return Ok(());
        }
        Err(Error::new(Code::NotAllowed, "irc.quit is not granted")
            .with_hint("the operator stops louro"))
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

fn not_allowed(message: impl Into<String>) -> Error {
    Error::new(Code::NotAllowed, message).with_hint("don't retry; the operator chose this")
}

#[cfg(test)]
mod tests {
    use super::*;
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
            assert!(grants.check_history(target, dm).is_ok(), "{target}");
        }
        for (target, dm) in [
            ("#lounge", &unknown),
            ("lz", &unknown),
            ("#bots,#ops", &unknown),
        ] {
            let error = grants.check_history(target, dm).unwrap_err();
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
        assert!(grants.check_topic("#BOTS").is_ok());
        for channel in ["#lounge", "#ops"] {
            let error = grants.check_topic(channel).unwrap_err();
            assert_eq!(error.code, Code::NotAllowed, "{channel}");
        }
    }

    #[test]
    fn looks_up_only_a_nick_and_only_when_irc_whois_grants() {
        let error = Grants::default().check_whois("luiz").unwrap_err();
        assert_eq!(error.message, "irc.whois is not granted");
        let grants = grants("irc.whois = true");
        assert!(grants.check_whois("luiz").is_ok());
        for target in ["#bots", "luiz,maria", "luiz\r\nQUIT", ""] {
            let error = grants.check_whois(target).unwrap_err();
            assert_eq!(error.code, Code::NotAllowed, "{target:?}");
        }
    }

    #[test]
    fn redacts_and_quits_only_when_granted() {
        let none = Grants::default();
        assert_eq!(none.check_redact().unwrap_err().code, Code::NotAllowed);
        let error = none.check_quit().unwrap_err();
        assert_eq!(error.code, Code::NotAllowed);
        assert_eq!(error.hint.as_deref(), Some("the operator stops louro"));
        let both = grants("irc.redact = true\nirc.quit = true");
        assert!(both.check_redact().is_ok() && both.check_quit().is_ok());
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
