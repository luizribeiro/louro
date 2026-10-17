//! Deny-by-default checks of what the agent asks for against louro.toml's `[grants]`.

use crate::config::Grants;
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
        dm: &DmHistory,
    ) -> Result<Outgoing> {
        self.check_target(&target, dm)?;
        Ok(Outgoing {
            text: text::clean_lines(text)?,
            target,
            kind,
        })
    }

    pub fn check_quit(&self) -> Result<()> {
        if self.irc.quit {
            return Ok(());
        }
        Err(Error::new(Code::NotAllowed, "irc.quit is not granted")
            .with_hint("the operator stops louro"))
    }

    fn check_target(&self, target: &str, dm: &DmHistory) -> Result<()> {
        // `PRIVMSG maria,#lounge` would also reach a target no grant covers.
        if target.contains(',') {
            return Err(not_allowed("send to one target at a time"));
        }
        if target.starts_with(['#', '&', '+', '!']) {
            if self.irc.join.matches(target) && self.irc.send.matches(target) {
                return Ok(());
            }
            return Err(not_allowed(format!("irc.send does not grant {target}")));
        }
        if target.starts_with(['@', '%', '~']) {
            return Err(not_allowed(format!(
                "{target} is a status message to a channel, not a nick"
            )));
        }
        // `$` starts a server mask, and keeps `$first` from naming a nick.
        if target.starts_with([':', '$']) {
            return Err(not_allowed(format!(
                "{target} is not a valid message target"
            )));
        }
        let dm_send = &self.irc.dm.send;
        let allowed = dm_send.matches_sender(target, dm.account.as_deref())
            || dm.first && dm_send.matches(FIRST);
        if !allowed {
            return Err(not_allowed(format!("irc.dm.send does not grant {target}")));
        }
        Ok(())
    }
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
        grants.check_send(target.into(), "hi", Kind::Message, dm)
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
