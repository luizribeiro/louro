# Changelog

All notable changes to louro are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and louro adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

The first release.

### Daemon and connection

- `louro connect` starts a daemon that holds one IRC connection through
  [papo](https://github.com/luizribeiro/papo): TLS, SASL, bot mode,
  multiline, reconnect with history to fill the gap, alternate nicks. The
  address is a host or an `irc://`, `ircs://`, `ws://` or `wss://` URL.
- The SASL password comes from the TTY prompt, stdin or a `password_file`,
  and only `connect` reads it. Nothing on the socket returns it, and a text
  that contains it is refused (`blocked_secret`).
- `connect` runs in the background unless given `--foreground`.
- `connect --replace` reads the config and password first, then stops the
  running daemon and starts in its place.
- The daemon quits on `louro quit` (with `irc.quit`), SIGTERM or SIGINT,
  and refuses to start while another one answers on its socket.

### Config and grants

- `louro.toml` with `[server]`, `[grants]`, `[limits]`, `[filter]` and
  `[typing]` tables. Everything not granted is denied.
- Grants for reading and speaking in channels and DMs (`irc.join`,
  `irc.send`, `irc.dm.read`, `irc.dm.send`), and for `react`, `typing`,
  `redact`, `topic`, `whois`, `list`, `watch`, `identity`, `quit` and
  `ask`. Names match case-insensitively with a trailing `*` glob;
  `account:NAME` matches the services account, checked with WHOIS when
  needed; `$first` allows DMs only to people who DMed the bot first.
  `irc.ask` takes `account:NAME` entries only.
- `[limits]` caps sends per target, sends in total and reactions per
  target, and how many characters of each incoming message the agent sees
  (`incoming_text`, 1000 by default; the rest is cut and marked `truncated`).
- `[filter]` runs an operator program on every text before it reaches IRC,
  to change or block it.
- `[typing]` shows typing to whoever a `wait` batch addressed, until the
  agent answers.
- `louro status` shows the connection, the grants and the limits.

### Agent commands

- `wait` blocks for events and moves the cursor only after the CLI has
  printed them, so nothing is missed or replayed. Events cover chat,
  reactions, redactions, invites, presence of watched nicks, and connection
  changes.
- The bot's own lines reach `wait` as `sent` events: every `send` through
  the daemon (`via: louro`) and lines from another client on its account
  (`via: other_client`), under the same read grants as chat. They never
  wake `wait` by themselves.
- `wait` rides out a daemon restart, and a restarted daemon backfills the
  chat since the last acked `wait` and reports it with a `started` event.
- `send` for messages, notices and actions, multiline from stdin, and
  threaded replies with `--reply-to`.
- `history` with `--before`, `--after` (msgid, time or `30m` ago) and
  `--grep` to search a conversation with a regex.
- `react`, `typing`, `redact`, `names`, `topic`, `whois`, `whowas`, `list`,
  `join`, `part`, `watch`, `nick`, `away` and `setname`.
- `ask` DMs a question and blocks until the person answers
  with one of the choices or the timeout passes.
- Bare `louro` and `louro --help` print a primer for the agent.

### Safety

- Outgoing text is checked too: a `send` is at most 5 lines and 1000
  characters.
- Grants, limits and the filter come only from the config; no socket
  request changes them.

### Output

- Every command prints one JSON object; results carry the current time in
  `now`. Errors carry a stable `code`, a `message` and often a `hint`, and
  map to fixed exit codes: 2 bad input, 3 no daemon, 4 IRC link down, 5 not
  allowed, 6 rate limited, 7 unsupported, 8 filter failed, 9 could not
  connect, 10 server error, 11 CLI and daemon versions differ.

### Packaging

- A Nix flake with a package and a `services.louro` NixOS module that runs
  louro as its own sandboxed user, with the password as a systemd
  credential.
- On macOS the flake signs louro with the hardened runtime, so another
  process of the same user can't read the password from its memory.

[Unreleased]: https://github.com/luizribeiro/louro/commits/main
