//! Deny-by-default checks of what the agent asks for against louro.toml's `[grants]`.

use crate::config::{Grants, Selector};
use crate::error::{Code, Error, Result};
use crate::irc::{Incoming, Kind, Outgoing};
use crate::text;

/// In `irc.dm.send`: anyone whose DM the daemon has received.
const FIRST: &str = "$first";

/// What the daemon knows about DMs received from a target nick, for `irc.dm.send`'s `$first`
/// and `account:` entries. Both come from the same nick→account bookkeeping (see `daemon.rs`).
#[derive(Default)]
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

    pub fn check_send(
        &self,
        target: String,
        text: &str,
        kind: Kind,
        reply_to: Option<String>,
        dm: &DmHistory,
    ) -> Result<Outgoing> {
        self.check_target(&target, dm)?;
        if let Some(msgid) = &reply_to {
            text::check_msgid(msgid)?;
        }
        Ok(Outgoing {
            text: text::clean_lines(text)?,
            target,
            kind,
            reply_to,
        })
    }

    /// Reacting needs `irc.react` for the target on top of the right to send there.
    pub fn check_react(
        &self,
        target: &str,
        msgid: &str,
        reaction: &str,
        dm: &DmHistory,
    ) -> Result<()> {
        self.check_also(&self.irc.react, "irc.react", target, dm)?;
        text::check_msgid(msgid)?;
        text::check_reaction(reaction)
    }

    /// Showing typing needs `irc.typing` for the target on top of the right to send there.
    pub fn check_typing(&self, target: &str, dm: &DmHistory) -> Result<()> {
        self.check_also(&self.irc.typing, "irc.typing", target, dm)
    }

    /// A grant that narrows sending, like `irc.react`: the target needs both.
    fn check_also(&self, grant: &Selector, name: &str, target: &str, dm: &DmHistory) -> Result<()> {
        self.check_target(target, dm)?;
        if grant.matches_sender(target, dm.account.as_deref()) {
            return Ok(());
        }
        Err(not_allowed(format!("{name} does not grant {target}")))
    }

    /// A channel's history needs `irc.join`; a DM conversation's needs `irc.dm.read` for the nick.
    pub fn check_history(&self, target: &str, dm: &DmHistory) -> Result<()> {
        match target_kind(target)? {
            TargetKind::Channel => self.check_join(target),
            TargetKind::Nick => {
                if self
                    .irc
                    .dm
                    .read
                    .matches_sender(target, dm.account.as_deref())
                {
                    return Ok(());
                }
                Err(not_allowed(format!("irc.dm.read does not grant {target}")))
            }
        }
    }

    /// Reading what a channel holds, like its members, needs `irc.join`.
    pub fn check_join(&self, channel: &str) -> Result<()> {
        if self.irc.join.matches(channel) {
            return Ok(());
        }
        Err(not_allowed(format!("irc.join does not grant {channel}")))
    }

    pub fn check_quit(&self) -> Result<()> {
        if self.irc.quit {
            return Ok(());
        }
        Err(Error::new(Code::NotAllowed, "irc.quit is not granted")
            .with_hint("the operator stops louro"))
    }

    fn check_target(&self, target: &str, dm: &DmHistory) -> Result<()> {
        match target_kind(target)? {
            TargetKind::Channel => {
                if self.irc.join.matches(target) && self.irc.send.matches(target) {
                    return Ok(());
                }
                Err(not_allowed(format!("irc.send does not grant {target}")))
            }
            TargetKind::Nick => {
                let dm_send = &self.irc.dm.send;
                let allowed = dm_send.matches_sender(target, dm.account.as_deref())
                    || dm.first && dm_send.matches(FIRST);
                if allowed {
                    return Ok(());
                }
                Err(not_allowed(format!("irc.dm.send does not grant {target}")))
            }
        }
    }
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
    // `$` starts a server mask, and keeps `$first` from naming a nick. A space would split the
    // target into more IRC parameters where papo doesn't check it, as in CHATHISTORY.
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
    use crate::irc::{Origin, Sender};

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
        incoming_as(target, nick, None, private)
    }

    fn incoming_as(target: &str, nick: &str, account: Option<&str>, private: bool) -> Incoming {
        Incoming {
            kind: Kind::Message,
            time: "2026-10-16T21:04:11.392Z".into(),
            msgid: None,
            target: target.into(),
            private,
            from: Sender {
                nick: nick.into(),
                account: account.map(String::from),
                bot: false,
            },
            text: "louro: ping".into(),
            mentions_me: true,
            reply_to: None,
            origin: Origin::Live,
            from_me: false,
        }
    }

    fn send(grants: &Grants, target: &str, dm_received: bool) -> Result<Outgoing> {
        send_as(
            grants,
            target,
            &DmHistory {
                first: dm_received,
                account: None,
            },
        )
    }

    fn send_as(grants: &Grants, target: &str, dm: &DmHistory) -> Result<Outgoing> {
        grants.check_send(target.into(), "hi", Kind::Message, None, dm)
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
        ] {
            let error = send(&grants, target, true).unwrap_err();
            assert_eq!(error.code, Code::NotAllowed, "{target}");
        }
    }

    #[test]
    fn dm_read_accepts_account_selectors() {
        let grants = grants(r#"irc.dm.read = ["account:luiz"]"#);
        for (nick, account, readable) in [
            ("lz", Some("luiz"), true),
            ("luiz", None, false),
            ("luiz", Some("mallory"), false),
        ] {
            let event = incoming_as("louro", nick, account, true);
            assert_eq!(grants.may_read(&event), readable, "{nick} ({account:?})");
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
            assert_eq!(send_as(&grants, "lz", &dm).is_ok(), allowed, "{account:?}");
        }
    }

    #[test]
    fn first_with_an_account_refuses_once_the_nick_no_longer_maps_to_it() {
        let grants = grants(r#"irc.dm.send = ["$first"]"#);
        let still_luiz = DmHistory {
            first: true,
            account: Some("luiz".into()),
        };
        assert!(send_as(&grants, "luiz", &still_luiz).is_ok());

        let changed_hands = DmHistory {
            first: false,
            account: None,
        };
        assert_eq!(
            send_as(&grants, "luiz", &changed_hands).unwrap_err().code,
            Code::NotAllowed
        );
    }

    #[test]
    fn reads_channel_state_only_where_irc_join_grants() {
        let grants = bots_and_dms();
        assert!(grants.check_join("#Lounge").is_ok());
        for target in ["#ops", "luiz"] {
            let error = grants.check_join(target).unwrap_err();
            assert_eq!(error.code, Code::NotAllowed, "{target}");
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
            grants.check_react(target, "4y8kq7j2c9mnz", "👍", &dm)
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
        let bad_msgid = grants.check_react("#bots", "4y8k q7j2", "👍", &DmHistory::default());
        assert_eq!(bad_msgid.unwrap_err().code, Code::Usage);
    }

    #[test]
    fn shows_typing_only_where_irc_typing_and_irc_send_both_grant() {
        let grants = grants(
            r##"
            irc.join = ["#bots", "#lounge"]
            irc.send = ["#bots"]
            irc.typing = ["#bots", "#lounge"]
            "##,
        );
        let dm = DmHistory::default();
        assert!(grants.check_typing("#bots", &dm).is_ok());
        for target in ["#lounge", "#ops", "luiz"] {
            let error = grants.check_typing(target, &dm).unwrap_err();
            assert_eq!(error.code, Code::NotAllowed, "{target}");
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
    fn quits_only_with_irc_quit() {
        assert!(grants("irc.quit = true").check_quit().is_ok());
        let error = Grants::default().check_quit().unwrap_err();
        assert_eq!(error.code, Code::NotAllowed);
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
