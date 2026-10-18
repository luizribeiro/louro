# The VM tests' shared part: a node "machine" running Ergo and people on IRC
# (irc-human.py), each an irc-human-NICK service logged in to its account, and
# a test script that runs irc.py, then script.
{
  settings,
  humans,
  script,
}:
{ config, lib, ... }:
let
  pkgs = config.node.pkgs;
  ircHuman = pkgs.writers.writePython3Bin "irc-human" { } ./irc-human.py;
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
  # The darwin host builds the guest through the Determinate Linux builder,
  # whose case-insensitive /nix/store breaks the systemd initrd's terminfo.
  defaults.boot.initrd.systemd.enable = lib.mkIf config.hostPkgs.stdenv.hostPlatform.isDarwin false;

  nodes.machine = {
    services.ergochat = {
      enable = true;
      settings = {
        server.listeners.":${toString settings.ircPort}" = { };
        # A client that disconnects leaves, unless its account opts in to always-on. nixpkgs's
        # ergochat module defaults this to "opt-out" (always-on unless an account opts out), so
        # this pins the suite to upstream-normal disconnect semantics.
        accounts.multiclient.always-on = "opt-in";
        # The test sends in bursts that fakelag would throttle.
        fakelag.enabled = false;
        # Without +t, a member who isn't an operator may set the topic.
        channels.default-modes = "+n";
        # Ergo offers draft/message-redaction only with this on.
        history.retention.allow-individual-delete = true;
        # Without these, Ergo remembers no one for WHOWAS and offers MONITOR=0; 100 is its
        # own default.yaml's.
        limits.whowas-entries = 100;
        limits.monitor-entries = 100;
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
    systemd.services = lib.mapAttrs' (
      nick: password: lib.nameValuePair "irc-human-${nick}" (humanService nick password)
    ) humans;
    environment.systemPackages = [ pkgs.curl ];
  };

  testScript = ''
    settings = ${builtins.toJSON settings}
    exec(compile(open("${./irc.py}").read(), "nix/tests/irc.py", "exec"))
    exec(compile(open("${script}").read(), "nix/tests/${baseNameOf script}", "exec"))
  '';
}
