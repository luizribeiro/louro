//! The IRC side of the daemon, over one papo client.

use std::future;

use papo::Client;

use crate::daemon::{Irc, IrcStatus, Outgoing};
use crate::error::{Code, Error, Result};
use crate::protocol::Kind;

pub struct PapoIrc {
    client: Client,
}

impl Irc for PapoIrc {
    fn status(&self) -> impl Future<Output = IrcStatus> + Send {
        let state = self.client.state();
        future::ready(IrcStatus {
            connected: state.is_connected(),
            nick: state.nick().to_string(),
            channels: state
                .channels()
                .map(|channel| channel.name().to_string())
                .collect(),
        })
    }

    async fn send(&self, message: Outgoing) -> Result<Option<String>> {
        if !self.client.state().is_connected() {
            return Err(Error::new(
                Code::NotConnected,
                "the IRC connection is down and papo is reconnecting",
            )
            .with_hint("retry in a minute"));
        }
        let Outgoing { target, kind, text } = message;
        let sent = match kind {
            Kind::Message => self.client.privmsg(&target, &text).await,
            Kind::Notice => self.client.notice(&target, &text).await,
            Kind::Action => self.client.privmsg(&target, &ctcp_action(&text)).await,
        };
        sent.map(|sent| sent.msgid)
            .map_err(|error| Error::new(send_code(&error), error.to_string()))
    }

    async fn quit(&self) {
        let _ = self.client.quit(None).await;
    }
}

/// A CTCP message is one line, so an action's lines are joined.
fn ctcp_action(text: &str) -> String {
    format!("\x01ACTION {}\x01", text.replace('\n', " "))
}

fn send_code(error: &papo::Error) -> Code {
    match error {
        papo::Error::Disconnected(_) => Code::NotConnected,
        papo::Error::Timeout => Code::RequestTimeout,
        papo::Error::Server(_) => Code::ServerError,
        papo::Error::InvalidInput(_) => Code::Usage,
        _ => Code::Internal,
    }
}

#[cfg(test)]
mod tests {
    use papo::{DisconnectCause, Disconnected, InvalidInput};

    use super::*;

    fn connection_lost() -> papo::Error {
        papo::Error::Disconnected(Disconnected::new(DisconnectCause::ConnectionLost, None))
    }

    #[test]
    fn an_action_is_one_ctcp_line() {
        assert_eq!(ctcp_action("waves"), "\x01ACTION waves\x01");
        assert_eq!(
            ctcp_action("checks the logs\nfinds nothing"),
            "\x01ACTION checks the logs finds nothing\x01"
        );
    }

    #[test]
    fn send_failures_map_to_louro_codes() {
        assert_eq!(send_code(&connection_lost()), Code::NotConnected);
        assert_eq!(send_code(&papo::Error::Timeout), Code::RequestTimeout);
        assert_eq!(
            send_code(&papo::Error::InvalidInput(InvalidInput::EmptyText)),
            Code::Usage
        );
        assert_eq!(send_code(&papo::Error::RequestLimit), Code::Internal);
    }
}
