# Configuration

louro reads `~/.config/louro/louro.toml` (or `$XDG_CONFIG_HOME/louro/louro.toml`,
or `connect --config PATH`). Only `connect` reads it. The agent can't change
it, and nothing it does over the socket changes what's granted.

```toml
[server]
address = "irc.example.org"     # TLS on 6697; or an irc://, ircs:// or wss:// URL
nick = "louro"
alt_nicks = ["louro_"]
sasl_username = "louro"         # connect prompts for the password
autojoin = ["#bots", "#lounge"]

[grants]                        # everything not granted is denied
irc.join = ["#bots", "#lounge", "#help-*"]
irc.send = ["#bots"]
irc.react = ["#bots"]
irc.dm.read = ["account:luiz", "maria"]
irc.dm.send = ["$first"]
irc.ask = ["account:luiz"]

[limits]
send = "3/10s"

[filter]
outgoing = ["/usr/local/bin/louro-redact", "--strict"]

[typing]
after_wait = "dms"
```

## `[server]`

| Key | What it is | Default |
|---|---|---|
| `address` | A host (TLS on 6697), or an `irc://`, `ircs://` or `wss://` URL. IPv6 goes in brackets. | required |
| `nick` | The bot's nick. | required |
| `alt_nicks` | Nicks to try when `nick` is taken. | none |
| `username`, `realname` | The bot's user name and real name. | papo's |
| `sasl_username` | The account to log in as. Turns SASL on. | `nick`, if `password_file` is set |
| `password_file` | Where to read the password, instead of prompting. | prompt |
| `autojoin` | Channels to join on connect. | none |
| `socket` | Where the daemon listens. | `$XDG_RUNTIME_DIR/louro/louro.sock`, else one in `$TMPDIR` |
| `state_dir` | Where louro keeps what outlives a run (the watch list, what the agent has seen). | `~/.local/state/louro` |

## `[grants]`

A grant is `true`, `false`, or a list of names:

- Names match case-insensitively, and one trailing `*` matches a prefix (`#help-*`).
- `account:NAME` matches a person by account. The server verifies accounts, but a nick can
  change hands, so prefer it for DMs.
- `$first`, in `irc.dm.send`, matches only people who DM'd the bot first.

| Grant | Lets the agent | Value |
|---|---|---|
| `irc.join` | sit and read in these channels, or `louro join` them | list |
| `irc.send` | speak in these channels; must be within `irc.join` | list |
| `irc.dm.read` | get DMs from these people | list |
| `irc.dm.send` | DM these people | list |
| `irc.react` | react to messages here; needs send too | list |
| `irc.typing` | show typing here; needs send too | list |
| `irc.redact` | delete its own messages | bool |
| `irc.topic` | set these channels' topic; reading needs only `irc.join` | list |
| `irc.whois` | look people up with `whois` and `whowas` | bool |
| `irc.list` | list the server's channels | bool |
| `irc.watch` | watch these nicks come, go and go away | list |
| `irc.identity` | change its nick, away status and realname | bool |
| `irc.quit` | stop the daemon with `louro quit` | bool |
| `irc.ask` | `louro ask` these people; `account:NAME` only (see [ask](ask.md)) | list |

`louro status` shows what's granted.

## `[limits]`

Caps on how fast the bot talks, a backstop against runaway loops. A limit
is `"COUNT/WINDOW"`, or `"off"`.

| Key | Caps | Default |
|---|---|---|
| `send` | messages per target (channel or nick) | `"3/10s"` |
| `send_total` | messages across all targets | `"10/1m"` |
| `react` | reactions per target | `"10/1m"` |

Typing has no limit.

## `[filter]`

A program of yours that sees everything the bot would show others before
it reaches IRC, say to catch pasted tokens.

| Key | What it is | Default |
|---|---|---|
| `outgoing` | The program and its arguments. No shell. | none |
| `timeout` | How long it may take. | `"5s"` |

- It gets the text on stdin, and context in env vars, each unset when it doesn't apply:
  `LOURO_KIND` (`message`, `notice`, `action`, `topic`, `part`, `redact`, `react`,
  `unreact`, `nick`, `away`, `realname` or `ask`), `LOURO_CHANNEL`, `LOURO_NICK`,
  `LOURO_ACCOUNT`, `LOURO_MSGID`, `LOURO_SELF_NICK`, `LOURO_SELF_ACCOUNT`,
  `LOURO_SERVER` and `LOURO_FILTER_VERSION=1`.
- Exit 0 sends its stdout (one trailing newline trimmed).
- Exit 10 blocks the text.
- Anything else sends nothing and tells the agent to ask you: another exit code, a
  timeout, or output that is empty, not UTF-8 or over 64 KiB.
- A short stderr reaches the agent as a note, and always goes to louro's log.
- It runs as louro's user, in `/`. `connect` refuses to start if it isn't an executable file.

## `[typing]`

Shows the bot typing while the agent works on an answer.

| Key | What it is | Default |
|---|---|---|
| `after_wait` | `"off"`; `"dms"` shows typing to each person whose DM `wait` just handed over; `"dms+mentions"` also covers channels where the bot was mentioned | `"off"` |
| `timeout` | When to stop if the agent hasn't answered. | `"60s"` |

Typing stops when the agent sends or reacts there, calls `wait` again, or
times out. Each target needs `irc.typing`.
