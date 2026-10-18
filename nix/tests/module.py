# services.louro's scenarios, run by module.nix's VM test after irc.py, with `settings` from
# module.nix.
import json
import shlex

BOT = settings["bot"]
SOCKET = settings["socket"]
CREDENTIAL = "/run/credentials/louro.service/password"
NO_SESSION = 3


def as_user(user, command):
    return shlex.join(["runuser", "-u", user, "--", *command])


def louro_as(user, *args):
    """Runs louro as user with LOURO_SOCKET set; returns its exit status and JSON output."""
    command = as_user(user, ["env", f"LOURO_SOCKET={SOCKET}", "louro", *args])
    status, out = machine.execute(command)
    return status, json.loads(out)


def status():
    code, out = louro_as("agent", "status")
    assert code == 0, out
    return out


def joined(out):
    return out["connected"] and "#bots" in out["channels"]


def bot_sent(command, times):
    """Waits until luiz has seen the bot send this IRC command this many times."""

    def count():
        log = machine.succeed(f"cat {human_file('luiz', 'log')}").splitlines()
        lines = [line for _tags, line in map(split_tags, log)]
        return sum(line.startswith(f":{BOT['user']}!") and f" {command} " in line for line in lines)

    until(count, lambda seen: seen == times)


start_ergo({BOT["user"]: BOT["password"]} | settings["humans"])
machine.succeed("systemctl start irc-human-luiz")
machine.wait_until_succeeds(f"grep -q ' 366 luiz #bots ' {human_file('luiz', 'log')}")
machine.succeed(
    f"install -D -m 400 /dev/null {settings['passwordFile']}",
    f"echo {shlex.quote(BOT['password'])} > {settings['passwordFile']}",
)
machine.succeed("systemctl start louro")

with subtest("louro logs in with SASL using the password from passwordFile"):
    until(status, joined)
    send_raw("luiz", f"WHOIS {BOT['user']}")
    received("luiz", lambda _tags, line: f" 330 luiz {BOT['user']} {BOT['user']} " in line)

with subtest("the service is sandboxed"):
    # systemd-analyze rates exposure from 0.0 to 10.0, and takes the threshold times ten.
    machine.succeed("systemd-analyze security --threshold=20 louro")

with subtest("only the socket's group may use it"):
    machine.succeed(f"stat -c '%a %G' {SOCKET} | grep -qx '770 louro'")
    code, out = louro_as("stranger", "status")
    assert code == NO_SESSION and out["error"]["code"] == "no_session", out
    assert "Permission denied" in out["error"]["message"], out

with subtest("the agent can read neither the credential nor its source"):
    for path in [CREDENTIAL, settings["passwordFile"]]:
        machine.succeed(f"test -s {path}")
        machine.fail(as_user("agent", ["cat", path]))

with subtest("a restart reconnects"):
    bot_sent("JOIN", 1)
    machine.succeed("systemctl restart louro")
    bot_sent("QUIT", 1)
    bot_sent("JOIN", 2)
    until(status, joined)

with subtest("stopping louro quits IRC and removes the socket"):
    machine.succeed("systemctl stop louro")
    bot_sent("QUIT", 2)
    machine.fail(f"test -e {SOCKET}")
    machine.succeed("systemctl show -p Result louro | grep -qx Result=success")
