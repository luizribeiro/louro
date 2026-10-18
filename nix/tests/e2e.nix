# One VM running Ergo and louro, driven by e2e.py: louro as an unprivileged
# user with a louro.toml per scenario, and people on IRC (irc-human.py).
{ pkgs, packages }:

let
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
in
pkgs.testers.runNixOSTest {
  name = "louro-e2e";

  imports = [
    (import ./irc.nix {
      inherit settings;
      script = ./e2e.py;
      # irc-human-louro logs in to the bot's account, to opt it in to always-on.
      humans = settings.humans // {
        ${settings.bot.user} = settings.bot.password;
      };
    })
  ];

  nodes.machine =
    { pkgs, ... }:
    {
      users.users.${settings.bot.user}.isNormalUser = true;
      environment.systemPackages = [ packages.${pkgs.stdenv.hostPlatform.system}.louro ];
    };
}
