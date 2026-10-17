//! Deny-by-default checks of what the agent asks for against louro.toml's `[grants]`.

use crate::config::Grants;
use crate::error::{Code, Error, Result};
use crate::irc::{Incoming, Kind, Outgoing};
use crate::text;

impl Grants {
    /// Channel chat only from `irc.join` channels, DMs only from `irc.dm.read` senders.
    pub fn may_read(&self, incoming: &Incoming) -> bool {
        if incoming.private {
            self.irc.dm.read.matches(&incoming.from.nick)
        } else {
            self.irc.join.matches(&incoming.target)
        }
    }

    pub fn check_send(&self, target: String, text: &str, kind: Kind) -> Result<Outgoing> {
        self.check_target(&target)?;
        Ok(Outgoing {
            text: text::clean_lines(text)?,
            target,
            kind,
        })
    }

    fn check_target(&self, target: &str) -> Result<()> {
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
        if target.starts_with(':') {
            return Err(not_allowed(format!(
                "{target} is not a valid message target"
            )));
        }
        if !self.irc.dm.send.matches(target) {
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
    use crate::irc::Sender;

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
            kind: Kind::Message,
            time: "2026-10-16T21:04:11.392Z".into(),
            msgid: None,
            target: target.into(),
            private,
            from: Sender {
                nick: nick.into(),
                account: None,
                bot: false,
            },
            text: "louro: ping".into(),
            mentions_me: true,
            reply_to: None,
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
            assert!(
                grants
                    .check_send(target.into(), "hi", Kind::Message)
                    .is_ok()
            );
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
        ] {
            let error = grants
                .check_send(target.into(), "hi", Kind::Message)
                .unwrap_err();
            assert_eq!(error.code, Code::NotAllowed, "{target}");
        }
    }

    #[test]
    fn grants_nothing_by_default() {
        let none = Grants::default();
        assert!(!none.may_read(&incoming("#bots", "maria", false)));
        assert!(!none.may_read(&incoming("luiz", "luiz", true)));
        for target in ["#bots", "luiz"] {
            assert!(none.check_send(target.into(), "hi", Kind::Message).is_err());
        }
    }
}
