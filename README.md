<p align="center"><img src="docs/banner.png" alt="louro, a robot parrot: a CLI for agents to chat on IRC" width="800"></p>

# louro

louro lets any AI coding agent with a shell act as an IRC bot. You run a
daemon that holds the one IRC connection; the agent just calls `louro wait`
and `louro send` in a loop. ("Louro" is Brazilian for a pet parrot.)

## Features

- Works with any agent that has a shell: Claude Code, Codex, pi, …
- The agent never holds the IRC password; only the daemon does
- Deny-by-default grants in a config file: which channels it may read and
  speak in, DMs (including `$first`, reply only to people who DM'd first),
  where it may react or show typing, and whether it may redact its own
  messages or quit
- Untrusted-text marking and sanitizing, as a defense against prompt
  injection from chat
- Nothing is missed between calls: a cursor plus an ack, not a poll
- Built on [papo](https://github.com/luizribeiro/papo) and IRCv3: SASL, bot
  mode, multiline, reconnect

## Quick start

1. Install it:

   ```
   nix run github:luizribeiro/louro -- --help
   # or
   cargo install --git https://github.com/luizribeiro/louro
   ```

2. Write a minimal `~/.config/louro/louro.toml`:

   ```toml
   [server]
   address = "irc.example.org"
   nick = "louro"
   sasl_username = "louro"
   autojoin = ["#bots"]

   [grants]
   irc.join = ["#bots"]
   irc.send = ["#bots"]
   ```

3. Run `louro connect` in your own terminal. It prompts for the password.

4. Tell your agent something like:

   > louro is connected to irc.example.org as louro. Run `louro --help`,
   > then watch #bots and answer questions people ask you.

5. From there the agent just waits and sends (trimmed; see `louro --help`
   for the real shape):

   ```
   $ louro wait
   {"events": [{"type": "message", "target": "#bots", "from": {"nick": "maria"},
     "text": "louro: what's rust's borrow checker?", "untrusted": true, …}], …}
   $ louro send '#bots' "it checks that references don't outlive what they point to"
   ```

## Config

`louro.toml` has two tables: `[server]` says what the bot connects as,
`[grants]` says what the agent may do. Everything not granted is denied. A
grant is `true` (everyone/everywhere), `false`, or a list of names matched
case-insensitively, with one trailing `*` allowed as a prefix glob; for DM
grants, prefer an `account:NAME` entry over a bare nick, since the server
verifies accounts but nicks can be taken by someone else.

<details>
<summary>A fuller example</summary>

```toml
[server]
address = "irc.example.org"
nick = "louro"
alt_nicks = ["louro_", "louro__"]
sasl_username = "louro"
# password_file = "/run/credentials/louro.service/password"
autojoin = ["#bots", "#lounge"]

[grants]
irc.join = ["#bots", "#lounge"]     # channels it may sit in and read
irc.send = ["#bots"]                # channels it may speak in; must be within irc.join
irc.dm.read = ["account:luiz", "maria"]  # whose DMs reach the agent
irc.dm.send = ["$first"]            # whom it may DM; $first = only people whose DM it received
# irc.quit = true                   # let the agent run `louro quit`
```

</details>

## Security

Chat is untrusted input. louro limits what an injected agent can do (the
grants above), but it can't stop the injection itself, so:

- Use a dedicated, low-value bot account you can revoke.
- Keep the password away from the agent: type it at the `connect` prompt
  in your own terminal (the default), or, for an always-on bot, run the
  daemon as its own user or outside the agent's VM with `password_file`
  (e.g. systemd's `LoadCredential=`). Either way, limiting the agent's
  shell to louro (e.g. Claude Code's `Bash(louro:*)`) adds another layer.
- Start `louro connect` in a separate terminal of your own, never from
  inside the agent's session.

## Status

Early. louro is built for LLMs, not scripts: output, error codes and config
can change without notice, and there's no backwards compatibility.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
