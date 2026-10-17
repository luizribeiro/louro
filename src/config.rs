//! `louro.toml`: `[server]` says what the bot connects as, `[grants]` what the agent may do.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Code, Error, Result};

const EXAMPLE: &str = "a minimal louro.toml:
[server]
address = \"irc.example.net\"
nick = \"louro\"
autojoin = [\"#bots\"]

[grants]
irc.join = [\"#bots\"]
irc.send = [\"#bots\"]";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: Server,
    #[serde(default)]
    pub grants: Grants,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    /// A host (TLS on 6697), or an `irc://`, `ircs://` or `wss://` URL; IPv6 goes in brackets.
    pub address: String,
    pub nick: String,
    #[serde(default)]
    pub alt_nicks: Vec<String>,
    pub username: Option<String>,
    pub realname: Option<String>,
    sasl_username: Option<String>,
    pub password_file: Option<PathBuf>,
    #[serde(default)]
    pub autojoin: Vec<String>,
    pub socket: Option<PathBuf>,
}

impl Server {
    /// SASL is on when a password file or SASL account is set; the account defaults to the nick.
    pub fn sasl_username(&self) -> Option<&str> {
        self.sasl_username
            .as_deref()
            .or(self.password_file.as_ref().map(|_| self.nick.as_str()))
    }
}

/// Everything not granted is denied.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Grants {
    pub irc: Irc,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Irc {
    /// Channels it may sit and read in.
    pub join: Selector,
    /// Channels it may speak in.
    pub send: Selector,
    pub dm: Dm,
    /// Channels and nicks whose messages it may react to; it must also be allowed to send there.
    pub react: Selector,
    /// Channels and nicks it may show typing to; it must also be allowed to send there.
    pub typing: Selector,
    /// Whether it may redact messages it sent itself.
    pub redact: bool,
    pub quit: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Dm {
    /// Whose DMs reach the agent.
    pub read: Selector,
    /// Whom it may DM.
    pub send: Selector,
}

/// `true` for everyone, or a list of names, each matched case-insensitively and allowed one
/// trailing `*`. In a DM selector (see [`Selector::matches_sender`]), an entry can also be
/// `account:NAME` to match the sender's logged-in account instead of their nick.
#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged, expecting = "true, false, or a list of names")]
pub enum Selector {
    All(bool),
    Only(Vec<String>),
}

impl Default for Selector {
    fn default() -> Self {
        Self::All(false)
    }
}

impl Selector {
    pub fn matches(&self, name: &str) -> bool {
        match self {
            Self::All(all) => *all,
            Self::Only(patterns) => patterns.iter().any(|pattern| glob_matches(pattern, name)),
        }
    }

    /// Like `matches`, but an `account:NAME` entry matches only a sender logged into that
    /// account (case-insensitively, no glob); other entries keep matching `nick`, as `matches`
    /// does. A sender with no account never matches an `account:` entry.
    pub fn matches_sender(&self, nick: &str, account: Option<&str>) -> bool {
        let by_account = account.is_some_and(|account| {
            matches!(self, Self::Only(patterns) if patterns.iter().any(|pattern| {
                pattern
                    .strip_prefix("account:")
                    .is_some_and(|wanted| wanted.eq_ignore_ascii_case(account))
            }))
        });
        by_account || self.matches(nick)
    }
}

fn glob_matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix)),
        None => pattern.eq_ignore_ascii_case(name),
    }
}

/// Reads `path`, else the default louro.toml; `socket` overrides `server.socket`. `--config` can
/// name any file, so errors never quote it, and a missing file reads like an unreadable one.
pub fn load(path: Option<&Path>, socket: Option<&Path>) -> Result<Config> {
    let path = match path {
        Some(path) => path.to_path_buf(),
        None => default_path()?,
    };
    let text = fs::read_to_string(&path).map_err(|_| {
        Error::new(Code::Usage, format!("can't read {}", path.display())).with_hint(EXAMPLE)
    })?;
    let mut config = parse(&text)
        .map_err(|err| Error::new(Code::Usage, format!("{}: {err}", path.display())))?;
    if let Some(socket) = socket {
        config.server.socket = Some(socket.to_path_buf());
    }
    Ok(config)
}

/// `$XDG_CONFIG_HOME/louro/louro.toml`, else `~/.config/louro/louro.toml`, on macOS too.
fn default_path() -> Result<PathBuf> {
    let dir = env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::home_dir().map(|home| home.join(".config")))
        .ok_or_else(|| {
            Error::new(
                Code::Usage,
                "there is no home directory to find louro.toml in",
            )
            .with_hint("pass --config PATH")
        })?;
    Ok(dir.join("louro/louro.toml"))
}

fn parse(text: &str) -> Result<Config, String> {
    let config: Config = toml::from_str(text).map_err(|err| describe(&err, text))?;
    let irc = &config.grants.irc;
    within_join(irc, &config.server.autojoin, "autojoin")?;
    if let Selector::Only(send) = &irc.send {
        within_join(irc, send, "irc.send")?;
    }
    Ok(config)
}

/// toml's message and position, without the source line its `Display` quotes.
fn describe(err: &toml::de::Error, text: &str) -> String {
    let message = without_value(err.message());
    let Some(before) = err.span().and_then(|span| text.get(..span.start)) else {
        return message;
    };
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    format!("line {line}, column {column}: {message}")
}

/// serde's type errors quote the value they found, as in `invalid type: string "…", expected a
/// sequence`, so only what was expected is kept.
fn without_value(message: &str) -> String {
    match message.split_once(": ") {
        Some((kind @ ("invalid type" | "invalid value"), found)) => {
            found.rsplit_once(", expected ").map_or_else(
                || kind.to_owned(),
                |(_, expected)| format!("{kind}, expected {expected}"),
            )
        }
        _ => message.to_owned(),
    }
}

/// The bot can't read or speak where it may not sit.
fn within_join(irc: &Irc, channels: &[String], setting: &str) -> Result<(), String> {
    match channels.iter().find(|channel| !irc.join.matches(channel)) {
        Some(channel) => Err(format!(
            "{channel} is in {setting} but not granted by irc.join"
        )),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        [server]
        address = "irc.weblounge.org"
        nick = "louro"
    "#;

    fn refusal(text: &str) -> String {
        parse(text).unwrap_err()
    }

    #[test]
    fn a_minimal_config_grants_nothing() {
        let Config { server, grants } = parse(MINIMAL).unwrap();
        assert_eq!(server.nick, "louro");
        assert_eq!(server.sasl_username(), None);
        assert!(server.autojoin.is_empty());
        let irc = grants.irc;
        for selector in [
            &irc.join,
            &irc.send,
            &irc.dm.read,
            &irc.dm.send,
            &irc.react,
            &irc.typing,
        ] {
            assert!(!selector.matches("#bots") && !selector.matches("maria"));
        }
        assert!(!irc.redact && !irc.quit);
    }

    #[test]
    fn refuses_autojoin_or_irc_send_outside_irc_join() {
        for (settings, refused) in [
            (
                "autojoin = [\"#bots\", \"#lounge\"]\n[grants]\nirc.join = [\"#bots\"]",
                "#lounge is in autojoin",
            ),
            (
                "[grants]\nirc.join = [\"#help-*\"]\nirc.send = [\"#*\"]",
                "#* is in irc.send",
            ),
        ] {
            let refusal = refusal(&format!("{MINIMAL}{settings}"));
            assert_eq!(refusal, format!("{refused} but not granted by irc.join"));
        }
    }

    #[test]
    fn a_config_error_gives_its_position_but_never_the_value_found() {
        for (settings, refused) in [
            (
                "autojoin = \"sk-proj-4fQe9TzXb2LmN8vR1sWk\"",
                "line 5, column 16: invalid type, expected a sequence",
            ),
            (
                "realname = \"Lóuro\" the bot",
                "line 5, column 24: unexpected key or value, expected newline, `#`",
            ),
        ] {
            assert_eq!(refusal(&format!("{MINIMAL}{settings}")), refused);
        }
    }

    #[test]
    fn account_selectors_match_only_a_sender_logged_into_that_account() {
        let selector = Selector::Only(vec!["account:luiz".into()]);
        for (nick, account, matches) in [
            ("lz", Some("luiz"), true),
            ("lz", Some("Luiz"), true),
            ("luiz", None, false),
            ("luiz", Some("mallory"), false),
        ] {
            assert_eq!(
                selector.matches_sender(nick, account),
                matches,
                "{nick} ({account:?})"
            );
        }
        let bare = Selector::Only(vec!["maria".into()]);
        assert!(bare.matches_sender("maria", Some("mallory")));
    }

    #[test]
    fn selectors_match_names_case_insensitively_with_a_trailing_glob() {
        let selector = Selector::Only(vec!["#Bots".into(), "#help-*".into(), "luiz".into()]);
        for name in ["#bots", "#BOTS", "#help-", "#HELP-rust", "Luiz"] {
            assert!(selector.matches(name), "{name}");
        }
        for name in ["#bots2", "#lounge", "#help", "luiz_", "maria", "#h€€"] {
            assert!(!selector.matches(name), "{name}");
        }
        assert!(Selector::All(true).matches("maria"));
        assert!(!Selector::All(false).matches("maria"));
    }
}
