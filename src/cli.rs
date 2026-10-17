use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "louro",
    version,
    about = "Let a coding agent act as an IRC bot"
)]
pub struct Cli {
    /// Unix socket to the daemon
    #[arg(long, global = true, env = "LOURO_SOCKET", value_name = "PATH")]
    pub socket: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Connection state, nick, channels
    Status,
    /// Block until matching events arrive
    Wait(WaitArgs),
    /// Send a message, notice or action
    Send(SendArgs),
    /// Disconnect and stop the daemon
    Quit,
    /// Start the daemon (operator only; not wired up yet)
    Connect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeout {
    Secs(u64),
    None,
}

impl Timeout {
    #[must_use]
    pub const fn as_secs(self) -> Option<u64> {
        match self {
            Self::Secs(secs) => Some(secs),
            Self::None => None,
        }
    }
}

fn parse_timeout(raw: &str) -> Result<Timeout, String> {
    if raw == "none" {
        return Ok(Timeout::None);
    }
    raw.parse()
        .map(Timeout::Secs)
        .map_err(|_| format!("expected a number of seconds or `none`, got `{raw}`"))
}

#[derive(Debug, Args)]
pub struct WaitArgs {
    /// Seconds to block for, or `none` to wait forever; `0` polls
    #[arg(long, default_value = "60", value_parser = parse_timeout, value_name = "SECS|none")]
    pub timeout: Timeout,
    /// Maximum number of events to return
    #[arg(long, default_value_t = 20, value_name = "N")]
    pub max: usize,
}

#[derive(Debug, Args)]
pub struct SendArgs {
    /// Channel or nick to send to
    pub target: String,
    /// Text to send, or `-` to read it from stdin (multi-line)
    pub text: String,
    /// Send as a NOTICE instead of a message
    #[arg(long, conflicts_with = "action")]
    pub notice: bool,
    /// Send as a CTCP ACTION ("/me ...")
    #[arg(long)]
    pub action: bool,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn parses_global_socket_before_the_subcommand() {
        let cli = Cli::try_parse_from(["louro", "--socket", "/tmp/louro.sock", "status"]).unwrap();
        assert_eq!(cli.socket, Some(PathBuf::from("/tmp/louro.sock")));
        assert!(matches!(cli.command, Command::Status));
    }

    #[test]
    fn wait_timeout_accepts_seconds_zero_and_none() {
        let cli = Cli::try_parse_from(["louro", "wait", "--timeout", "5"]).unwrap();
        let Command::Wait(args) = cli.command else {
            panic!("expected wait");
        };
        assert_eq!(args.timeout.as_secs(), Some(5));
        assert_eq!(args.max, 20);

        let cli = Cli::try_parse_from(["louro", "wait", "--timeout", "0"]).unwrap();
        let Command::Wait(args) = cli.command else {
            panic!("expected wait");
        };
        assert_eq!(args.timeout.as_secs(), Some(0));

        let cli = Cli::try_parse_from(["louro", "wait", "--timeout", "none"]).unwrap();
        let Command::Wait(args) = cli.command else {
            panic!("expected wait");
        };
        assert_eq!(args.timeout.as_secs(), None);
    }

    #[test]
    fn wait_timeout_defaults_to_sixty_seconds() {
        let cli = Cli::try_parse_from(["louro", "wait"]).unwrap();
        let Command::Wait(args) = cli.command else {
            panic!("expected wait");
        };
        assert_eq!(args.timeout.as_secs(), Some(60));
    }

    #[test]
    fn wait_timeout_rejects_garbage() {
        let err = Cli::try_parse_from(["louro", "wait", "--timeout", "soon"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn send_notice_and_action_are_mutually_exclusive() {
        let err = Cli::try_parse_from(["louro", "send", "#bots", "hi", "--notice", "--action"])
            .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn send_accepts_notice_or_action_alone() {
        let cli = Cli::try_parse_from(["louro", "send", "#bots", "hi", "--notice"]).unwrap();
        let Command::Send(args) = cli.command else {
            panic!("expected send");
        };
        assert!(args.notice);
        assert!(!args.action);
    }

    #[test]
    fn a_bad_flag_is_a_usage_error() {
        let err = Cli::try_parse_from(["louro", "status", "--nope"]).unwrap_err();
        assert!(err.use_stderr());
    }

    #[test]
    fn a_missing_subcommand_is_a_usage_error() {
        let err = Cli::try_parse_from(["louro"]).unwrap_err();
        assert!(err.use_stderr());
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
