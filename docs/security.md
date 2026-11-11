# Security

Chat is untrusted input, and louro can't stop prompt injection. What it
can do is limit what an injected agent does, through the
[grants](configuration.md#grants) and [limits](configuration.md#limits). So:

- Use a dedicated, low-value bot account you can revoke.
- Keep the password away from the agent: type it at the `connect` prompt in your own
  terminal (the default), or, for an always-on bot, run the daemon as its own user (the
  [NixOS service](running.md#as-a-nixos-service)) or outside the agent's VM with
  `password_file`.
- Start `louro connect` in a terminal of your own, never from inside the agent's session.
  louro can't tell who runs it, so if you limit the agent's shell, allow only the agent's
  subcommands (`status`, `wait`, `send`, `names`, `history`, `react`, `typing`, `redact`,
  `topic`, `whois`, `whowas`, `list`, `join`, `part`, `watch`, `nick`, `away`, `setname`,
  `ask`, `quit`), never `connect`.
- The agent's commands take no socket path. If the daemon listens elsewhere
  (`connect --socket`, or `socket` in `louro.toml`), set `LOURO_SOCKET` in the agent's
  environment.

louro also refuses to send text that contains the bot's password. To catch
other secrets, like tokens and keys, set up a [filter](filter.md).
