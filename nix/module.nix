# services.louro: the louro daemon as its own (dynamic) user. The agent gets
# the socket through group membership, never the password.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.louro;
  credential = "/run/credentials/louro.service/password";
  configFile = (pkgs.formats.toml { }).generate "louro.toml" (
    lib.recursiveUpdate cfg.settings (
      lib.optionalAttrs (cfg.passwordFile != null) { server.password_file = credential; }
    )
  );
in
{
  options.services.louro = {
    enable = lib.mkEnableOption "louro, the daemon that lets an AI agent act as an IRC bot";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.louro;
      defaultText = lib.literalExpression "louro.packages.\${pkgs.stdenv.hostPlatform.system}.louro";
      description = "The louro package to run.";
    };

    settings = lib.mkOption {
      inherit (pkgs.formats.toml { }) type;
      default = { };
      example = lib.literalExpression ''
        {
          server = {
            address = "irc.example.org";
            nick = "louro";
            autojoin = [ "#bots" ];
          };
          grants.irc = {
            join = [ "#bots" ];
            send = [ "#bots" ];
          };
        }
      '';
      description = ''
        louro.toml, as `[server]`, `[grants]` and `[limits]`; louro validates it at start.
        Set the password with {option}`services.louro.passwordFile`, not
        `server.password_file`, and the socket with {option}`services.louro.socket`.
      '';
    };

    passwordFile = lib.mkOption {
      type = lib.types.nullOr (
        lib.types.pathWith {
          inStore = false;
          absolute = true;
        }
      );
      default = null;
      example = "/run/secrets/louro-password";
      description = ''
        The bot's SASL password on the host, e.g. a sops-nix or agenix secret readable by root.
        systemd hands it to louro as a credential, so neither louro's user nor the agent's can
        read the source file. Null means no SASL; `server.sasl_username` without it fails, since
        a service has no terminal to prompt for the password on.
      '';
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "louro";
      description = ''
        The group whose members may use louro's socket. Give the agent access by adding its
        user to this group and setting `LOURO_SOCKET` to {option}`services.louro.socket` in
        its environment.
      '';
    };

    socket = lib.mkOption {
      type = lib.types.str;
      default = "/run/louro/louro.sock";
      description = ''
        The Unix socket louro listens on. Keep it in /run/louro, the runtime directory only the
        group may enter; elsewhere the group may be unable to reach it. The agent's commands
        find it through `LOURO_SOCKET`.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = !(cfg.settings ? server.password_file);
        message = "services.louro: set passwordFile, not settings.server.password_file";
      }
    ];

    users.groups.${cfg.group} = { };
    environment.systemPackages = [ cfg.package ];

    systemd.services.louro = {
      description = "louro, an IRC connection for an AI agent";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      serviceConfig = {
        ExecStart = lib.escapeShellArgs [
          (lib.getExe cfg.package)
          "connect"
          "--foreground"
          "--config"
          configFile
          "--socket"
          cfg.socket
        ];
        LoadCredential = lib.optional (cfg.passwordFile != null) "password:${cfg.passwordFile}";
        # louro exits once papo gives up reconnecting.
        Restart = "on-failure";
        DynamicUser = true;
        # systemd refuses a dynamic user named like an existing group, such as the static one.
        User = "louro-daemon";
        Group = cfg.group;
        RuntimeDirectory = "louro";
        RuntimeDirectoryMode = "0750";
        # louro binds its socket with this umask, and connecting needs write access to it.
        UMask = "0007";

        CapabilityBoundingSet = "";
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        NoNewPrivileges = true;
        PrivateDevices = true;
        PrivateTmp = true;
        PrivateUsers = true;
        ProcSubset = "pid";
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHome = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "invisible";
        ProtectSystem = "strict";
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        RestrictNamespaces = true;
        RestrictRealtime = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
          "~@resources"
        ];
      };
    };
  };
}
