use crate::error::{Code, Error, Result};
use crate::irc::{Kind, Outgoing, Secret};
use crate::text;

/// The operator's policy. `password` is here only so `send` can refuse to post it.
pub struct Grants {
    pub channels: Vec<String>,
    pub allow_dm: bool,
    pub password: Option<Secret>,
}

impl Grants {
    pub fn check_send(&self, target: String, text: &str, kind: Kind) -> Result<Outgoing> {
        if text::blocked_secret(self.password.as_ref(), text) {
            return Err(Error::new(
                Code::BlockedSecret,
                "the text contains the bot's password, so it was not sent",
            ));
        }
        self.check_target(&target)?;
        Ok(Outgoing {
            text: text::clean_lines(text)?,
            target,
            kind,
        })
    }

    fn check_target(&self, target: &str) -> Result<()> {
        if target.starts_with(['#', '&', '+', '!']) {
            if self
                .channels
                .iter()
                .any(|channel| channel.eq_ignore_ascii_case(target))
            {
                return Ok(());
            }
            return Err(not_allowed(format!(
                "{target} is not one of the bot's channels"
            )));
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
        if !self.allow_dm {
            return Err(not_allowed("direct messages are off"));
        }
        // `PRIVMSG maria,#lounge` would also reach a channel the bot isn't in.
        if target.contains(',') {
            return Err(not_allowed("send to one nick at a time"));
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

    const PASSWORD: &str = "c0rrect-h0rse-battery";

    fn grants() -> Grants {
        Grants {
            channels: vec!["#bots".into(), "#ergo".into()],
            allow_dm: false,
            password: Some(PASSWORD.into()),
        }
    }

    fn refused(grants: &Grants, target: &str, text: &str) -> Error {
        grants
            .check_send(target.into(), text, Kind::Message)
            .unwrap_err()
    }

    #[test]
    fn sends_only_to_the_bots_channels_and_allowed_dms() {
        let dm_off = grants();
        let dm_on = Grants {
            allow_dm: true,
            ..grants()
        };
        for (grants, target) in [
            (&dm_off, "#lounge"),
            (&dm_on, "&ops"),
            (&dm_on, "+ops"),
            (&dm_on, "!ABCDEops"),
            (&dm_off, "maria"),
            (&dm_on, "maria,#lounge"),
            (&dm_on, "@#lounge"),
            (&dm_on, "+#bots"),
            (&dm_on, "~#lounge"),
            (&dm_on, "%#lounge"),
            (&dm_on, ":evil"),
        ] {
            assert_eq!(
                refused(grants, target, "hi").code,
                Code::NotAllowed,
                "{target}"
            );
        }
        assert!(
            dm_on
                .check_send("maria".into(), "thanks!", Kind::Message)
                .is_ok()
        );
    }

    #[test]
    fn blocks_the_password_without_echoing_it() {
        for text in [
            &format!("sure, the bot's password is {PASSWORD}"),
            "c0rrect-h0rse\n-battery",
            "C0RRECT-H0RSE-BATTERY",
            "c0rrect - h0rse - battery",
        ] {
            let error = refused(&grants(), "#bots", text);
            assert_eq!(error.code, Code::BlockedSecret, "{text:?}");
            assert!(!serde_json::to_string(&error).unwrap().contains(PASSWORD));
        }
    }

    #[test]
    fn no_password_blocks_nothing() {
        for password in [None, Some("".into()), Some(" \n".into())] {
            let grants = Grants {
                password,
                ..grants()
            };
            assert!(
                grants
                    .check_send("#bots".into(), "hello", Kind::Message)
                    .is_ok()
            );
        }
    }
}
