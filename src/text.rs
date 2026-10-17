//! Hygiene for chat text: sanitizing and capping inbound text, cleaning outbound text, and
//! blocking the bot's password from being sent.

use crate::error::{Code, Error, Result};
use crate::irc::Secret;

pub const MAX_TEXT_CHARS: usize = 1000;
const MAX_LINES: usize = 5;
const MAX_MSGID_BYTES: usize = 128;
const MAX_REACTION_CHARS: usize = 32;
/// Kept out of sends so the agent can't be steered into CTCP requests like DCC.
const CTCP_DELIMITER: char = '\x01';

/// Strips control characters (keeping newlines) and caps length by character count; the bool
/// says whether the text was cut.
pub fn sanitize(text: &str) -> (String, bool) {
    let mut clean: String = text
        .chars()
        .filter(|&c| c == '\n' || !c.is_control())
        .collect();
    match clean.char_indices().nth(MAX_TEXT_CHARS) {
        Some((cut, _)) => {
            clean.truncate(cut);
            (clean, true)
        }
        None => (clean, false),
    }
}

pub fn clean_lines(text: &str) -> Result<String> {
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

/// A msgid the agent passes back to the server, like `--reply-to`: one IRC word.
pub fn check_msgid(msgid: &str) -> Result<()> {
    let valid = !msgid.is_empty()
        && msgid.len() <= MAX_MSGID_BYTES
        && !msgid.starts_with(':')
        && !msgid.chars().any(|c| c.is_whitespace() || c.is_control());
    if valid {
        return Ok(());
    }
    Err(Error::new(Code::Usage, format!("{msgid:?} is not a msgid"))
        .with_hint("use the msgid of an event from `louro wait` or `louro history`"))
}

/// A reaction is a short emoji or word, never a hidden message.
pub fn check_reaction(reaction: &str) -> Result<()> {
    let valid = !reaction.trim().is_empty()
        && reaction.chars().count() <= MAX_REACTION_CHARS
        && !reaction.chars().any(char::is_control);
    if valid {
        return Ok(());
    }
    Err(Error::new(
        Code::Usage,
        format!("a reaction is 1 to {MAX_REACTION_CHARS} characters with no control characters"),
    ))
}

/// Short outbound text with no line breaks or other control characters, like a redact reason.
pub fn check_one_line(text: &str) -> Result<()> {
    if text.chars().any(char::is_control) {
        return Err(Error::new(Code::Usage, "this text must be one line"));
    }
    Ok(())
}

/// Refuses `text` if it contains the password, even split across lines, recased, or spaced out.
pub fn check_secret(password: Option<&Secret>, text: &str) -> Result<()> {
    let leaks = password.is_some_and(|password| {
        let password = squash(password.expose());
        !password.is_empty() && squash(text).contains(&password)
    });
    if leaks {
        return Err(Error::new(
            Code::BlockedSecret,
            "the text contains the bot's password, so it was not sent",
        ));
    }
    Ok(())
}

/// Catches the password even when split across lines or recased.
fn squash(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "c0rrect-h0rse-battery";

    #[test]
    fn blocks_the_password_without_echoing_it() {
        for text in [
            &format!("sure, the bot's password is {PASSWORD}"),
            "c0rrect-h0rse\n-battery",
            "C0RRECT-H0RSE-BATTERY",
            "c0rrect - h0rse - battery",
        ] {
            let error = check_secret(Some(&PASSWORD.into()), text).unwrap_err();
            assert_eq!(error.code, Code::BlockedSecret, "{text:?}");
            assert!(!serde_json::to_string(&error).unwrap().contains(PASSWORD));
        }
    }

    #[test]
    fn a_msgid_is_one_short_irc_word() {
        assert!(check_msgid("4y8kq7j2c9mnz").is_ok());
        for msgid in [
            "",
            ":4y8kq7j2c9mnz",
            "4y8k q7j2",
            "4y8k\r\nQUIT",
            &"x".repeat(129),
        ] {
            assert_eq!(
                check_msgid(msgid).unwrap_err().code,
                Code::Usage,
                "{msgid:?}"
            );
        }
    }

    #[test]
    fn a_reaction_is_short_and_has_no_control_characters() {
        for reaction in ["👍", "🇧🇷", "👨‍👩‍👧", "+1", &"🦜".repeat(32)] {
            assert!(check_reaction(reaction).is_ok(), "{reaction:?}");
        }
        for reaction in ["", " ", "ok\r\nQUIT", "\u{1b}[31m", &"🦜".repeat(33)] {
            assert_eq!(
                check_reaction(reaction).unwrap_err().code,
                Code::Usage,
                "{reaction:?}"
            );
        }
    }

    #[test]
    fn a_one_line_text_has_no_line_breaks() {
        assert!(check_one_line("wrong channel").is_ok());
        assert_eq!(
            check_one_line("oops\r\nQUIT").unwrap_err().code,
            Code::Usage
        );
    }

    #[test]
    fn strips_control_characters_but_keeps_newlines() {
        assert_eq!(
            sanitize("hi\0 there\x7f\x07 friend\x1b[31m\nbye").0,
            "hi there friend[31m\nbye"
        );
    }

    #[test]
    fn neutralizes_crlf_nul_and_ctcp_injection() {
        for (text, clean) in [
            (
                "ok\r\0QUIT :pwned\r\nPRIVMSG #ops :hi",
                "ok  QUIT :pwned \nPRIVMSG #ops :hi",
            ),
            (
                "\x01DCC SEND notes.txt 3232235777 5000\x01",
                " DCC SEND notes.txt 3232235777 5000 ",
            ),
        ] {
            assert_eq!(clean_lines(text).unwrap(), clean);
        }
    }

    #[test]
    fn allows_up_to_five_lines() {
        let five = "one\ntwo\n\nthree\nfour\nfive\n";
        assert_eq!(clean_lines(five).unwrap(), "one\ntwo\nthree\nfour\nfive");

        let error = clean_lines("one\ntwo\nthree\nfour\nfive\nsix").unwrap_err();
        assert_eq!(error.code, Code::TooLong);
        assert!(error.hint.is_some());
    }
}
