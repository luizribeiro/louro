//! `louro.toml`: `[server]` says what the bot connects as, `[grants]` what the agent may do.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

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
    /// A host (TLS on 6697), or an `irc://`, `ircs://` or `wss://` URL.
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
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Grants {
    pub irc: Irc,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Irc {
    /// Channels it may sit and read in.
    pub join: Selector,
    /// Channels it may speak in.
    pub send: Selector,
    pub dm: Dm,
    pub quit: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Dm {
    /// Whose DMs reach the agent.
    pub read: Selector,
    /// Whom it may DM.
    pub send: Selector,
}

/// `true` for everyone, or a list of names, each matched case-insensitively and allowed one
/// trailing `*`.
#[derive(Debug, Deserialize)]
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
}

fn glob_matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix)),
        None => pattern.eq_ignore_ascii_case(name),
    }
}

/// Reads `path`, else the default louro.toml; `socket` overrides `server.socket`.
pub fn load(path: Option<&Path>, socket: Option<&Path>) -> Result<Config> {
    let path = match path {
        Some(path) => path.to_path_buf(),
        None => default_path()?,
    };
    let text = fs::read_to_string(&path).map_err(|err| {
        Error::new(Code::Usage, format!("can't read {}: {err}", path.display())).with_hint(EXAMPLE)
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
    let config: Config = toml::from_str(text).map_err(|err| err.to_string())?;
    let irc = &config.grants.irc;
    within_join(irc, &config.server.autojoin, "autojoin")?;
    if let Selector::Only(send) = &irc.send {
        within_join(irc, send, "irc.send")?;
    }
    Ok(config)
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

    const FULL: &str = r##"
        [server]
        address = "ircs://irc.weblounge.org:6697"
        nick = "louro"
        alt_nicks = ["louro_", "louro__"]
        username = "louro"
        realname = "louro, an agent on IRC"
        sasl_username = "louro-bot"
        password_file = "/run/credentials/louro.service/password"
        autojoin = ["#bots", "#lounge"]
        socket = "/run/louro/louro.sock"

        [grants]
        irc.join = ["#bots", "#lounge", "#help-*"]
        irc.send = ["#bots"]
        irc.dm.read = true
        irc.dm.send = ["$first", "luiz"]
        irc.quit = true
    "##;

    const MINIMAL: &str = r#"
        [server]
        address = "irc.weblounge.org"
        nick = "louro"
    "#;

    fn refusal(text: &str) -> String {
        parse(text).unwrap_err()
    }

    #[test]
    fn reads_a_full_config() {
        let Config { server, grants } = parse(FULL).unwrap();
        assert_eq!(server.address, "ircs://irc.weblounge.org:6697");
        assert_eq!(server.alt_nicks, ["louro_", "louro__"]);
        assert_eq!(server.realname.as_deref(), Some("louro, an agent on IRC"));
        assert_eq!(server.sasl_username(), Some("louro-bot"));
        assert_eq!(server.autojoin, ["#bots", "#lounge"]);
        assert_eq!(server.socket, Some("/run/louro/louro.sock".into()));
        let irc = grants.irc;
        assert!(irc.join.matches("#help-rust") && irc.send.matches("#bots"));
        assert!(!irc.send.matches("#lounge"));
        assert!(irc.dm.read.matches("maria") && irc.dm.send.matches("luiz"));
        assert!(irc.quit);
    }

    #[test]
    fn a_minimal_config_grants_nothing() {
        let Config { server, grants } = parse(MINIMAL).unwrap();
        assert_eq!(server.nick, "louro");
        assert_eq!(server.sasl_username(), None);
        assert!(server.autojoin.is_empty());
        let irc = grants.irc;
        for selector in [&irc.join, &irc.send, &irc.dm.read, &irc.dm.send] {
            assert!(!selector.matches("#bots") && !selector.matches("maria"));
        }
        assert!(!irc.quit);
    }

    #[test]
    fn a_password_file_turns_on_sasl_as_the_nick() {
        let text = format!("{MINIMAL}password_file = \"/run/louro/password\"");
        assert_eq!(parse(&text).unwrap().server.sasl_username(), Some("louro"));
    }

    #[test]
    fn refuses_unknown_keys() {
        for typo in [
            "[server]\naddres = \"irc.weblounge.org\"\nnick = \"louro\"",
            &format!("{MINIMAL}\n[grants]\nirc.joni = [\"#bots\"]"),
            &format!("{MINIMAL}\n[grants]\nirc.dm.write = true"),
            &format!("{MINIMAL}\n[grant]\nirc.quit = true"),
        ] {
            assert!(refusal(typo).contains("unknown field"), "{typo}");
        }
    }

    #[test]
    fn refuses_a_selector_that_is_not_true_or_a_list() {
        let text = format!("{MINIMAL}\n[grants]\nirc.join = \"#bots\"");
        assert!(refusal(&text).contains("true, false, or a list of names"));
    }

    #[test]
    fn refuses_autojoin_outside_irc_join() {
        let text = r##"
            [server]
            address = "irc.weblounge.org"
            nick = "louro"
            autojoin = ["#bots", "#lounge"]

            [grants]
            irc.join = ["#bots"]
        "##;
        assert_eq!(
            refusal(text),
            "#lounge is in autojoin but not granted by irc.join"
        );
    }

    #[test]
    fn refuses_irc_send_outside_irc_join() {
        let text = format!("{MINIMAL}\n[grants]\nirc.join = [\"#help-*\"]\nirc.send = [\"#*\"]");
        assert_eq!(
            refusal(&text),
            "#* is in irc.send but not granted by irc.join"
        );
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

    #[test]
    fn the_socket_flag_overrides_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("louro.toml");
        fs::write(
            &path,
            format!("{MINIMAL}socket = \"/run/louro/louro.sock\""),
        )
        .unwrap();
        let config = load(Some(&path), Some(Path::new("/tmp/louro.sock"))).unwrap();
        assert_eq!(config.server.socket, Some("/tmp/louro.sock".into()));
    }

    #[test]
    fn a_missing_file_is_a_usage_error_with_an_example() {
        let error = load(Some(Path::new("/nonexistent/louro.toml")), None).unwrap_err();
        assert_eq!(error.code, Code::Usage);
        assert!(error.message.contains("/nonexistent/louro.toml"));
        assert!(error.hint.unwrap().contains("[grants]"));
    }
}
