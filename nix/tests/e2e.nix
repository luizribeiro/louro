# One VM running Ergo and louro, driven by e2e.py: louro as an unprivileged
# user with a louro.toml per scenario.
{ pkgs, packages }:

pkgs.testers.runNixOSTest (
  { config, lib, ... }:
  let
    guestPkgs = config.node.pkgs;
    inherit (guestPkgs.stdenv.hostPlatform) system;
    settings = {
      apiToken = "e2e-api-token";
      apiPort = 8089;
      ircPort = 6667;
      bot = {
        user = "louro";
        password = "squawk-squawk";
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
          # A client that disconnects leaves, rather than lingering as always-on.
          accounts.multiclient.always-on = "disabled";
          # The test sends in bursts that fakelag would throttle.
          fakelag.enabled = false;
          api = {
            enabled = true;
            listener = "127.0.0.1:${toString settings.apiPort}";
            bearer-tokens = [ settings.apiToken ];
          };
        };
      };
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
