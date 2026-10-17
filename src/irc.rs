//! The IRC side of the daemon: one papo client, with its chat forwarded as [`Incoming`].

use papo::{
    Client, ConnectErrorKind, Endpoint, Event, Events, Message, MessageKind, RegistrationError,
    Sasl, Source, State,
};
use tokio::sync::mpsc;

pub use papo::Secret;

use crate::cli::ConnectArgs;
use crate::daemon::{Incoming, Irc, IrcStatus, Outgoing, Sender};
use crate::error::{Code, Error, Result};
use crate::protocol::Kind;

const INCOMING_CAPACITY: usize = 256;

pub struct PapoIrc {
    client: Client,
}

/// Connects, registers, and forwards chat from the joined channels and DMs.
pub async fn connect(
    args: &ConnectArgs,
    password: Option<Secret>,
) -> Result<(PapoIrc, mpsc::Receiver<Incoming>)> {
    let endpoint = if args.plaintext {
        Endpoint::plaintext(&args.server, args.port())
    } else {
        Endpoint::tls(&args.server, args.port())
    };
    let mut builder = Client::builder(endpoint)
        .nick(&args.nick)
        .bot()
        .autojoin(args.joins.clone());
    if let Some(password) = password {
        let user = args.sasl_user.as_ref().unwrap_or(&args.nick);
        builder = builder.sasl(Sasl::any(user, password));
    }
    let (client, events) = builder
        .connect()
        .await
        .map_err(|error| Error::new(connect_code(&error), error.to_string()))?;
    let (chat, incoming) = mpsc::channel(INCOMING_CAPACITY);
    tokio::spawn(forward(client.clone(), events, chat));
    Ok((PapoIrc { client }, incoming))
}

fn connect_code(error: &papo::Error) -> Code {
    match error {
        papo::Error::Registration(
            RegistrationError::Sasl { .. }
            | RegistrationError::SaslUnavailable
            | RegistrationError::PasswordIncorrect,
        ) => Code::AuthFailed,
        papo::Error::Connect(failure)
            if matches!(
                failure.kind(),
                ConnectErrorKind::Tls | ConnectErrorKind::Certificate | ConnectErrorKind::TlsConfig
            ) =>
        {
            Code::Tls
        }
        _ => Code::ConnectFailed,
    }
}

async fn forward(client: Client, mut events: Events, chat: mpsc::Sender<Incoming>) {
    while let Some(event) = events.next().await {
        let Event::Message(message) = event else {
            continue;
        };
        let Some(incoming) = to_incoming(&message, &client.state()) else {
            continue;
        };
        if chat.send(incoming).await.is_err() {
            return;
        }
    }
}

fn to_incoming(message: &Message, state: &State) -> Option<Incoming> {
    if !message.is_live() {
        return None;
    }
    let kind = chat_kind(message.kind())?;
    let nick = message.sender.as_ref().and_then(Source::nick)?;
    let private = state.is_me(message.target());
    if !private && state.channel(message.target()).is_none() {
        return None;
    }
    Some(Incoming {
        kind,
        time: humantime::format_rfc3339_millis(message.time()).to_string(),
        msgid: message.msgid().map(str::to_owned),
        target: message.reply_target().to_owned(),
        private,
        from: Sender {
            nick: nick.to_owned(),
            account: message.account().map(str::to_owned),
            bot: message.is_bot(),
        },
        text: message.text().to_owned(),
        mentions_me: private
            || message
                .text()
                .to_lowercase()
                .contains(&state.nick().to_lowercase()),
        reply_to: message.reply_to().map(str::to_owned),
    })
}

fn chat_kind(kind: MessageKind) -> Option<Kind> {
    match kind {
        MessageKind::Privmsg => Some(Kind::Message),
        MessageKind::Notice => Some(Kind::Notice),
        MessageKind::Action => Some(Kind::Action),
        _ => None,
    }
}

impl Irc for PapoIrc {
    fn status(&self) -> IrcStatus {
        let state = self.client.state();
        IrcStatus {
            connected: state.is_connected(),
            nick: state.nick().to_string(),
            channels: state
                .channels()
                .map(|channel| channel.name().to_string())
                .collect(),
        }
    }

    async fn send(&self, message: Outgoing) -> Result<Option<String>> {
        let Outgoing { target, kind, text } = message;
        let sent = match kind {
            Kind::Message => self.client.privmsg(&target, &text).await,
            Kind::Notice => self.client.notice(&target, &text).await,
            Kind::Action => self.client.privmsg(&target, &ctcp_action(&text)).await,
        };
        sent.map(|sent| sent.id.map(|id| id.msgid().to_owned()))
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

    #[test]
    fn an_action_is_one_ctcp_line() {
        assert_eq!(
            ctcp_action("checks the logs\nfinds nothing"),
            "\x01ACTION checks the logs finds nothing\x01"
        );
    }

    #[test]
    fn papo_errors_map_to_louro_codes() {
        let lost = Disconnected::new(DisconnectCause::ConnectionLost, None);
        for (error, code) in [
            (papo::Error::Disconnected(lost), Code::NotConnected),
            (papo::Error::Timeout, Code::RequestTimeout),
            (
                papo::Error::InvalidInput(InvalidInput::EmptyText),
                Code::Usage,
            ),
        ] {
            assert_eq!(send_code(&error), code);
        }
        let refused = papo::Error::Registration(RegistrationError::SaslUnavailable);
        assert_eq!(connect_code(&refused), Code::AuthFailed);
    }

    #[test]
    fn keeps_chat_and_drops_other_ctcp() {
        for (kind, chat) in [
            (MessageKind::Privmsg, Some(Kind::Message)),
            (MessageKind::Notice, Some(Kind::Notice)),
            (MessageKind::Action, Some(Kind::Action)),
            (MessageKind::Ctcp, None),
        ] {
            assert_eq!(chat_kind(kind), chat);
        }
    }
}
