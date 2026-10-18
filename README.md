<p align="center"><img src="docs/banner.png" alt="louro, a robot parrot: a CLI for agents to chat on IRC" width="800"></p>

# louro

louro lets any AI coding agent with a shell act as an IRC bot. You run a
daemon that holds the one IRC connection; the agent just calls `louro wait`
and `louro send` in a loop. ("Louro" is Brazilian for a pet parrot.)

## Features

- Works with any agent that has a shell: Claude Code, Codex, pi, …
- The agent never holds the IRC password; only the daemon does
- Deny-by-default grants in a config file: where it may read, speak,
  react or DM (`$first`: only people who DM'd it first), and what else it
  may do
- Chat is sanitized and marked untrusted, against prompt injection
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

`louro.toml` has three tables: `[server]` says what the bot connects as,
`[grants]` what the agent may do, `[limits]` how much. Everything not
granted is denied. A grant is `true`, `false`, or a list of names matched
case-insensitively, with one trailing `*` as a prefix glob. For DM grants,
prefer `account:NAME` over a bare nick: the server verifies accounts, but
a nick can change hands. `[limits]` caps sends and reactions, a backstop
against runaway loops, retries and prompt-injected spam; `louro status`
shows them.

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
irc.join = ["#bots", "#lounge", "#help-*"]  # channels it may sit in and read, or `louro join`
irc.send = ["#bots"]                # channels it may speak in; must be within irc.join
irc.dm.read = ["account:luiz", "maria"]  # whose DMs reach the agent
irc.dm.send = ["$first"]            # whom it may DM; $first = only people whose DM it received
# irc.topic = ["#bots"]             # channels whose topic it may set; reading needs only irc.join
# irc.whois = true                  # let it look people up with `louro whois` and `louro whowas`
# irc.list = true                   # let it list the server's channels with `louro list`
# irc.watch = ["luiz"]              # nicks it may watch come and go with `louro watch`
# irc.identity = true               # let it change its nick, away status and realname
# irc.quit = true                   # let the agent run `louro quit`

[limits]
send = "3/10s"         # per target (channel or nick); default
send_total = "10/1m"   # across every target; default
react = "10/1m"        # per target; default; typing has no limit
# send = "off"         # any limit can be turned off
```

</details>

## Run it as a NixOS service

The flake's `nixosModules.default` runs louro as its own user, with the
password handed over as a systemd credential. The agent's user joins the
`louro` group to use the socket, and never sees the password:

```nix
services.louro = {
  enable = true;
  settings = {
    server = {
      address = "irc.example.org";
      nick = "louro";
      autojoin = [ "#bots" ];
    };
    grants.irc = {
      join = [ "#bots" ];
      send = [ "#bots" ];
      dm.read = [ "account:luiz" ]; # the bot's admin may DM it
      dm.send = [ "account:luiz" ];
    };
  };
  passwordFile = config.sops.secrets.louro-password.path; # or agenix, or any root-only file
};
users.users.agent.extraGroups = [ config.services.louro.group ];
# The path isn't secret: only members of the louro group can connect to it.
environment.variables.LOURO_SOCKET = config.services.louro.socket;
```

## Security

Chat is untrusted input. louro limits what an injected agent can do (the
grants above), but it can't stop the injection itself, so:

- Use a dedicated, low-value bot account you can revoke.
- Keep the password away from the agent: type it at the `connect` prompt
  in your own terminal (the default), or, for an always-on bot, run the
  daemon as its own user (the NixOS service above) or outside the agent's
  VM with `password_file`.
- Start `louro connect` in a separate terminal of your own, never from
  inside the agent's session. louro can't tell who runs it, so if you
  limit the agent's shell, allow only the agent's subcommands (`status`,
  `wait`, `send`, `names`, `history`, `react`, `typing`, `redact`,
  `topic`, `whois`, `whowas`, `list`, `join`, `part`, `watch`, `nick`,
  `away`, `setname`, `quit`), never `connect`.
- The agent's commands take no socket path. If the daemon listens
  elsewhere (`connect --socket`, or `socket` in `louro.toml`), set
  `LOURO_SOCKET` in the agent's environment.

## Status

Early. louro is built for LLMs, not scripts: output, error codes and config
can change without notice, and there's no backwards compatibility.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
