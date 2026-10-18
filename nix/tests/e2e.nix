# One VM running Ergo and louro, driven by e2e.py: louro as an unprivileged
# user with a louro.toml per scenario, and people on IRC (irc-human.py).
{ pkgs, packages }:

pkgs.testers.runNixOSTest (
  { config, lib, ... }:
  let
    guestPkgs = config.node.pkgs;
    inherit (guestPkgs.stdenv.hostPlatform) system;
    ircHuman = guestPkgs.writers.writePython3Bin "irc-human" { } ./irc-human.py;
    settings = {
      apiToken = "e2e-api-token";
      apiPort = 8089;
      ircPort = 6667;
      bot = {
        user = "louro";
        password = "squawk-squawk";
      };
      humans = {
        luiz = "luiz-password";
        mallory = "mallory-password";
      };
    };
    humanService =
      nick: password:
      let
        dir = "irc-human-${nick}";
      in
      {
        description = "${nick}, on IRC";
        serviceConfig = {
          RuntimeDirectory = dir;
          ExecStart = lib.escapeShellArgs [
            (lib.getExe ircHuman)
            "127.0.0.1:${toString settings.ircPort}"
            nick
            "--password=${password}"
            "--join=#bots"
            "--fifo=/run/${dir}/in"
            "--log=/run/${dir}/log"
          ];
        };
      };
  in
  {
    name = "louro-e2e";

    # The darwin host builds the guest through the Determinate Linux builder,
    # whose case-insensitive /nix/store breaks the systemd initrd's terminfo.
    defaults.boot.initrd.systemd.enable = lib.mkIf pkgs.stdenv.hostPlatform.isDarwin false;

    nodes.machine = {
      services.ergochat = {
        enable = true;
        settings = {
          server.listeners.":${toString settings.ircPort}" = { };
          # A client that disconnects leaves, unless its account opts in to always-on.
          accounts.multiclient.always-on = "opt-in";
          # The test sends in bursts that fakelag would throttle.
          fakelag.enabled = false;
          # Without +t, a member who isn't an operator may set the topic.
          channels.default-modes = "+n";
          # Ergo offers draft/message-redaction only with this on.
          history.retention.allow-individual-delete = true;
          # Ergo remembers no one for WHOWAS without this; 100 is its own default.yaml's.
          limits.whowas-entries = 100;
          # Sized to Ergo's own default.yaml, so Ergo advertises draft/multiline for papo to negotiate.
          limits.multiline = {
            max-bytes = 4096;
            max-lines = 100;
          };
          api = {
            enabled = true;
            listener = "127.0.0.1:${toString settings.apiPort}";
            bearer-tokens = [ settings.apiToken ];
          };
        };
      };
      # irc-human-louro logs in to the bot's account, to opt it in to always-on.
      systemd.services = lib.mapAttrs' (
        nick: password: lib.nameValuePair "irc-human-${nick}" (humanService nick password)
      ) (settings.humans // { ${settings.bot.user} = settings.bot.password; });
      users.users.${settings.bot.user}.isNormalUser = true;
      environment.systemPackages = [
        packages.${system}.louro
        guestPkgs.curl
      ];
    };

    testScript = ''
      settings = ${builtins.toJSON settings}
      exec(compile(open("${./e2e.py}").read(), "nix/tests/e2e.py", "exec"))
    '';
  }
)
