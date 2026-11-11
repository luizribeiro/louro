<p align="center"><img src="docs/banner.png" alt="louro, a robot parrot: a CLI for agents to chat on IRC" width="800"></p>

# louro

louro lets any AI coding agent with a shell act as an IRC bot. You run a
daemon that holds the IRC connection and its password; the agent calls
`louro wait` and `louro send` in a loop and reads JSON back.

[![louro connect, then pi answering questions in #bots, seen from senpai](docs/demos/louro.svg)](https://asciinema.org/a/bT6nlIxgv52NoEQy)

## Features

- Works with any agent that has a shell: Claude Code, Codex, pi, …
- Works from scripts too: every command prints JSON and exits with a fixed code, so a
  bash loop can run a bot (see [scripting](docs/scripting.md))
- The agent never holds the IRC password; only the daemon does
- Deny-by-default grants: where it may read, speak, react or DM, and what else it may do
- More than chat: reactions, typing, threaded replies, history search, and `louro ask` to
  ask someone by DM and wait for the answer
- Built on [papo](https://github.com/luizribeiro/papo), a Rust IRCv3 client library

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

Use a dedicated bot account, and never run `louro connect` from inside the
agent's session. See [security](docs/security.md).

## Learn more

- [Configuration](docs/configuration.md): every grant, limit and option in `louro.toml`
- [Running louro](docs/running.md): restarts, backfill, and the NixOS service
- [Scripting](docs/scripting.md): drive louro from bash, and what each exit code means
- [Filtering](docs/filter.md): redact or block what the bot says before it reaches IRC
- [louro ask](docs/ask.md): ask someone a question by DM and wait for the answer
- [Security](docs/security.md): keeping the password and the daemon away from the agent

## Related projects

- [papo](https://github.com/luizribeiro/papo): the Rust IRCv3 client library louro is
  built on.
- [weblounge](https://github.com/luizribeiro/weblounge): the infrastructure for
  weblounge.org, an invite-only IRC network.

Built by Luiz Ribeiro ([l9o.dev](https://l9o.dev)).

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
