# services.louro in a VM with Ergo: louro as its own user with a password from
# passwordFile, an agent in its group, and a stranger outside it.
{ pkgs, module }:

let
  settings = {
    apiToken = "module-api-token";
    apiPort = 8089;
    ircPort = 6667;
    bot = {
      user = "louro";
      password = "squawk-squawk";
    };
    humans.luiz = "luiz-password";
    passwordFile = "/run/secrets/louro-password";
    socket = "/run/louro/louro.sock";
  };
in
pkgs.testers.runNixOSTest {
  name = "louro-module";

  imports = [
    (import ./irc.nix {
      inherit settings;
      inherit (settings) humans;
    })
  ];

  nodes.machine = {
    imports = [ module ];
    services.louro = {
      enable = true;
      settings = {
        server = {
          address = "irc://127.0.0.1:${toString settings.ircPort}";
          nick = settings.bot.user;
          autojoin = [ "#bots" ];
        };
        grants.irc = {
          join = [ "#bots" ];
          send = [ "#bots" ];
        };
      };
      inherit (settings) passwordFile;
    };
    # The test starts louro once Ergo knows the bot's account and the secret is in place.
    systemd.services.louro.wantedBy = pkgs.lib.mkForce [ ];
    users.users = {
      agent = {
        isNormalUser = true;
        extraGroups = [ "louro" ];
      };
      stranger.isNormalUser = true;
    };
  };

  testScript = ''
    settings = ${builtins.toJSON settings}
    exec(compile(open("${./irc.py}").read(), "nix/tests/irc.py", "exec"))
    exec(compile(open("${./module.py}").read(), "nix/tests/module.py", "exec"))
  '';
}
