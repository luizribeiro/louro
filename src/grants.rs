use crate::error::{Code, Error, Result};
use crate::irc::{Outgoing, Secret};
use crate::socket::SendArgs;

const MAX_LINES: usize = 5;
/// Kept out of sends so the agent can't be steered into CTCP requests like DCC.
const CTCP_DELIMITER: char = '\x01';

/// The operator's policy. `password` is here only so `send` can refuse to post it.
pub struct Grants {
    pub channels: Vec<String>,
    pub allow_dm: bool,
    pub password: Option<Secret>,
}

impl Grants {
    pub fn check_send(&self, args: SendArgs) -> Result<Outgoing> {
        if self.contains_password(&args.text) {
            return Err(Error::new(
                Code::BlockedSecret,
                "the text contains the bot's password, so it was not sent",
            ));
        }
        self.check_target(&args.target)?;
        Ok(Outgoing {
            text: clean_lines(&args.text)?,
            target: args.target,
            kind: args.kind,
        })
    }

    fn contains_password(&self, text: &str) -> bool {
        self.password.as_ref().is_some_and(|password| {
            let password = squash(password.expose());
            !password.is_empty() && squash(text).contains(&password)
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

/// Catches the password even when split across lines or recased.
fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .flat_map(char::to_lowercase)
        .collect()
}

fn clean_lines(text: &str) -> Result<String> {
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| line.replace(['\r', '\0', CTCP_DELIMITER], " "))
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return Err(Error::new(Code::Usage, "there is no text to send"));
    }
    if lines.len() > MAX_LINES {
        return Err(Error::new(
            Code::TooLong,
            format!("{} lines is more than the {MAX_LINES} allowed", lines.len()),
        )
        .with_hint("shorten or summarize the reply"));
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::irc::Kind;

    const PASSWORD: &str = "c0rrect-h0rse-battery";

    fn grants() -> Grants {
        Grants {
            channels: vec!["#bots".into(), "#ergo".into()],
            allow_dm: false,
            password: Some(PASSWORD.into()),
        }
    }

    fn send(target: &str, text: &str) -> SendArgs {
        SendArgs {
            target: target.into(),
            text: text.into(),
            kind: Kind::Message,
        }
    }

    fn refused(grants: &Grants, target: &str, text: &str) -> Error {
        grants.check_send(send(target, text)).unwrap_err()
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
        assert!(dm_on.check_send(send("maria", "thanks!")).is_ok());
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
            assert!(grants.check_send(send("#bots", "hello")).is_ok());
        }
    }

    #[test]
    fn neutralizes_crlf_and_nul_injection() {
        let outgoing = grants()
            .check_send(send("#bots", "ok\r\0QUIT :pwned\r\nPRIVMSG #ops :hi"))
            .unwrap();
        assert_eq!(outgoing.text, "ok  QUIT :pwned \nPRIVMSG #ops :hi");
    }

    #[test]
    fn neutralizes_ctcp_requests() {
        let outgoing = grants()
            .check_send(send("#bots", "\x01DCC SEND notes.txt 3232235777 5000\x01"))
            .unwrap();
        assert_eq!(outgoing.text, " DCC SEND notes.txt 3232235777 5000 ");
    }

    #[test]
    fn allows_up_to_five_lines() {
        let five = "one\ntwo\n\nthree\nfour\nfive\n";
        let outgoing = grants().check_send(send("#bots", five)).unwrap();
        assert_eq!(outgoing.text, "one\ntwo\nthree\nfour\nfive");

        let error = refused(&grants(), "#bots", "one\ntwo\nthree\nfour\nfive\nsix");
        assert_eq!(error.code, Code::TooLong);
        assert!(error.hint.is_some());
    }
}
