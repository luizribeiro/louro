# Running louro

## Start, stop, restart

- `louro connect` reads the config and the password, connects, and goes to the background.
  `--foreground` keeps it in your terminal.
- `louro quit` (needs `irc.quit`) or SIGTERM stops it.
- `connect` refuses to start while one is running. To restart, say after upgrading, run
  `louro connect --replace`. It reads the config and password first, so a mistake there
  leaves the running louro alone. Then it stops the one on that socket (it must run as your
  user) and starts in its place.

## Restarts don't lose chat

- `wait` moves on only once the agent has its events, so nothing is dropped between calls.
- A `wait` blocked during a restart reconnects and carries on with the time it has left.
  It gives up (exit 3) once louro has been gone for 2 minutes.
- A restarted louro backfills the chat it missed, as far as the server's history allows
  (up to 24h). On Ergo, DMs sent while the bot is offline are kept only if its account
  is always-on.

## As a NixOS service

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

`settings` is [`louro.toml`](configuration.md) as Nix.
