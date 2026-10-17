use std::num::NonZeroUsize;
use std::path::Path;
use std::process::ExitCode;

use crate::output;
use crate::socket::{Connection, Op};

/// `none` is `u64::MAX` seconds, past any `Instant`, so the daemon sets no deadline.
fn parse_timeout(raw: &str) -> Result<u64, String> {
    if raw == "none" {
        return Ok(u64::MAX);
    }
    raw.parse()
        .map_err(|_| format!("expected a number of seconds or `none`, got `{raw}`"))
}

#[derive(clap::Args)]
pub struct Args {
    /// Seconds to block for, or `none` to wait forever; `0` polls
    #[arg(long, default_value = "60", value_parser = parse_timeout, value_name = "SECS|none")]
    pub timeout: u64,
    /// Maximum number of events to return
    #[arg(long, default_value = "20", value_name = "N")]
    pub max: NonZeroUsize,
}

impl Args {
    /// Acks only after printing, so the daemon never drops events the agent didn't see.
    pub async fn run(self, socket: Option<&Path>) -> ExitCode {
        let mut connection = match Connection::connect(socket).await {
            Ok(connection) => connection,
            Err(err) => return output::emit::<()>(Err(err)),
        };
        let result = connection
            .request(Op::Wait {
                timeout: self.timeout,
                max: self.max,
            })
            .await;
        let printed_ok = result.is_ok();
        let exit_code = output::emit(result);
        if printed_ok {
            let _ = connection.ack().await;
        }
        exit_code
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Test {
        #[command(flatten)]
        args: Args,
    }

    fn parse(flags: &[&str]) -> Args {
        Test::try_parse_from([&["wait"][..], flags].concat())
            .unwrap()
            .args
    }

    #[test]
    fn wait_timeout_takes_seconds_or_none() {
        for (flags, secs) in [(&[][..], 60), (&["--timeout", "none"], u64::MAX)] {
            assert_eq!(parse(flags).timeout, secs);
        }
    }
}
