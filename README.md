<p align="center"><img src="docs/banner.png" alt="louro, a robot parrot: a CLI for agents to chat on IRC" width="800"></p>

# louro

louro lets any AI coding agent with a shell act as an IRC bot. You run a
daemon that holds the IRC connection and its password; the agent calls
`louro wait` and `louro send` in a loop and reads JSON back. It may do only
what you grant it, and chat reaches it marked untrusted. ("Louro" is
Brazilian for a pet parrot.)

## Features

- Works with any agent that has a shell: Claude Code, Codex, pi, …
- The agent never holds the IRC password; only the daemon does
- Deny-by-default grants in a config file: where it may read, speak,
  react or DM (`$first`: only people who DM'd it first), and what else it
  may do
- Chat is sanitized and marked untrusted, against prompt injection
- Nothing is missed between calls: `wait` moves on only once the agent has
  its events, rides out a daemon restart, and a restarted daemon backfills
  the chat it missed where the server's history allows
- More than chat: typing, reacting, threaded replies, history search with
  `--grep`, and `louro ask` to ask someone by DM and wait for the answer
- Built on [papo](https://github.com/luizribeiro/papo), a Rust IRCv3
  client library, and doubles as its example app: SASL, bot mode,
  multiline, reconnect

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

`louro quit` (with `irc.quit`) or SIGTERM stops the daemon, and `connect`
refuses to start while one runs. To restart it, say after upgrading louro,
run `louro connect --replace`: it reads the config and password first, so a
mistake there leaves the running louro alone, then stops the one on that
socket (it must run as your user) and starts in its place. A `louro wait`
blocked meanwhile reconnects and carries on with the time it has left; it
gives up (exit 3) once louro has been gone for 2 minutes.

## Config

`louro.toml` has five tables: `[server]` says what the bot connects as,
`[grants]` what the agent may do, `[limits]` how much, the optional
`[filter]` what checks its words first, and the optional `[typing]` when
the bot shows typing on its own. Everything not
granted is denied. A grant is `true`, `false`, or a list of names matched
case-insensitively, with one trailing `*` as a prefix glob. For DM grants,
prefer `account:NAME` over a bare nick: the server verifies accounts, but
a nick can change hands. `[limits]` caps sends and reactions, a backstop
against runaway loops, retries and prompt-injected spam; `louro status`
shows them.

`[filter]` runs a program of yours on every text the bot would show
others before it reaches IRC, say to catch pasted tokens. It gets the
text on stdin and the context in `LOURO_*` variables, each unset when it
doesn't apply: `LOURO_KIND` (`message`, `notice`, `action`, `topic`,
`part`, `redact`, `react`, `unreact`, `nick`, `away`, `realname` or
`ask`),
`LOURO_CHANNEL`, `LOURO_NICK`, `LOURO_ACCOUNT`, `LOURO_MSGID`,
`LOURO_SELF_NICK`, `LOURO_SELF_ACCOUNT`, `LOURO_SERVER` and
`LOURO_FILTER_VERSION=1`. Exit 0 sends its stdout (one trailing newline
trimmed), checked again like the agent's own text; exit 10 blocks it.
Anything else, a timeout, or output that is empty, not UTF-8, over 64 KiB
or refused by those checks sends nothing, and the agent is told to ask
you. A short stderr is passed to the agent as a note (unless the filter
failed) and always logged on louro's stderr. It runs as louro's own user
(under the NixOS module, the service user, inside its systemd sandbox)
with `/` as its working directory. `connect` refuses to start if the
program isn't an executable
file.

`[typing]` shows the bot typing while the agent works on an answer.
With `after_wait = "dms"`, once the agent takes events from `louro wait`,
louro shows typing to each person whose DM was among them;
`"dms+mentions"` also covers each channel where a message mentioned the
bot. Notices and chat replayed after a reconnect don't count, and each
target needs `irc.typing`. It stops when the agent sends or reacts there
(or sets typing there itself), calls `wait` again, or after `timeout`
(60s by default). The default is `off`.

`louro ask NICK TEXT [--choices yes,no] [--timeout 5m]` (those are the
defaults) DMs `[ask k3fa] TEXT` and `reply !yes k3fa or !no k3fa`, then
blocks until they answer, say from a hook that asks you before an agent
runs a tool. It prints `{"id", "target", "answer", "timed_out"}`:
`"answer": "yes"`, or `null` with `"timed_out": true` once the timeout (at
most 1h) passes; both exit 0. If louro stops meanwhile, it exits 3
(`no_session`). You answer `!yes k3fa`, or a bare `!yes` when it's the
only pending ask to you and none closed in the last 10s; otherwise the bot
replies "which ask?". Answers never reach `wait`; if `irc.react` lists
you, the bot reacts 🫡 to your answer once it is recorded.

The nick must pass `irc.dm.send`, and `irc.ask` must list its
`account:NAME`: an answer counts only from a live DM whose account tag
matches. Since the agent could post a look-alike question for your answer
to land on, it can't send or react to you while an ask to you is pending.
The question goes out whole or not at all: too long is refused, and so is
anything the filter would change.

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
# state_dir = "/var/lib/louro"  # keeps the watch list across restarts; default ~/.local/state/louro

[grants]
irc.join = ["#bots", "#lounge", "#help-*"]  # channels it may sit in and read, or `louro join`
irc.send = ["#bots"]                # channels it may speak in; must be within irc.join
irc.dm.read = ["account:luiz", "maria"]  # whose DMs reach the agent
irc.dm.send = ["$first"]            # whom it may DM; $first = only people whose DM it received
# irc.topic = ["#bots"]             # channels whose topic it may set; reading needs only irc.join
# irc.whois = true                  # let it look people up with `louro whois` and `louro whowas`
# irc.list = true                   # let it list the server's channels with `louro list`
# irc.watch = ["luiz"]              # nicks it may watch come, go and go away with `louro watch`
# irc.identity = true               # let it change its nick, away status and realname
# irc.quit = true                   # let the agent run `louro quit`
# irc.ask = ["account:luiz"]        # whom `louro ask` may ask; account:NAME entries only

[limits]
send = "3/10s"         # per target (channel or nick); default
send_total = "10/1m"   # across every target; default
react = "10/1m"        # per target; default; typing has no limit
# send = "off"         # any limit can be turned off

# [filter]
# outgoing = ["/usr/local/bin/louro-redact", "--strict"]  # argv, no shell
# timeout = "5s"       # default

# [typing]
# after_wait = "dms"   # off (default), dms, or dms+mentions
# timeout = "60s"      # default
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
  `away`, `setname`, `ask`, `quit`), never `connect`.
- The agent's commands take no socket path. If the daemon listens
  elsewhere (`connect --socket`, or `socket` in `louro.toml`), set
  `LOURO_SOCKET` in the agent's environment.

## Status

louro is young and has one maintainer. Within 0.1.x:

- Command results and errors keep their JSON shapes, error codes and exit
  codes. New fields and new codes may appear; existing ones keep their
  meaning and exit code.
- Existing `louro.toml` files keep working; new fields are only added.
- The socket protocol between the CLI and the daemon is internal: upgrade
  both together. A mismatch exits 11 with a hint naming the stale side.
- `louro ask` is experimental: its flags, result and the DM it sends may
  still change.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
