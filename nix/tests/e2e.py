# The e2e scenarios, run by e2e.nix's VM test with `settings` from e2e.nix.
import datetime as dt
import json
import shlex

BOT = settings["bot"]
HOME = f"/home/{BOT['user']}"
CONFIG = f"{HOME}/louro.toml"
PASSWORD_FILE = f"{HOME}/password"
SOCKET = f"{HOME}/run/louro.sock"
SERVER = f"irc://127.0.0.1:{settings['ircPort']}"
DAEMON_RUNNING = f"pgrep -u {BOT['user']} -x louro"
TIMEOUT = dt.timedelta(minutes=2)
EXIT_CODES = {"not_allowed": 5}


def as_bot(command):
    return shlex.join(["runuser", "-u", BOT["user"], "--", *command])


def louro(*args, exit_code=0):
    """Runs louro as the bot's user; returns its JSON output."""
    command = as_bot(["env", f"LOURO_SOCKET={SOCKET}", "louro", *args])
    status, out = machine.execute(command)
    assert status == exit_code, f"{command}: exit {status}, wanted {exit_code}: {out}"
    return json.loads(out)


def refused(code, *args):
    """Runs louro, expecting it to fail with code and its exit code."""
    error = louro(*args, exit_code=EXIT_CODES[code])["error"]
    assert error["code"] == code, error


def register(account, password):
    request = json.dumps({"accountName": account, "passphrase": password})
    response = machine.succeed(shlex.join([
        "curl", "-sf", "-H", f"Authorization: Bearer {settings['apiToken']}",
        "-d", request, f"http://127.0.0.1:{settings['apiPort']}/v1/saregister",
    ]))
    assert json.loads(response)["success"], response


def toml_lines(table, prefix=""):
    for key, value in table.items():
        if isinstance(value, dict):
            yield from toml_lines(value, f"{prefix}{key}.")
        else:
            yield f"{prefix}{key} = {json.dumps(value)}"


def grants(dm_read=False, dm_send=False, quit=False):
    return {"irc": {
        "join": ["#bots"],
        "send": ["#bots"],
        "dm": {"read": dm_read, "send": dm_send},
        "quit": quit,
    }}


def write_as_bot(path, text):
    """Writes path as the bot's user, readable only by it."""
    write = as_bot(["sh", "-c", f"umask 077 && cat > {path}"])
    machine.succeed(f"echo {shlex.quote(text)} | {write}")


def connect(grants):
    """Starts the daemon with a fresh louro.toml and password file."""
    server = {
        "address": SERVER,
        "nick": BOT["user"],
        "password_file": PASSWORD_FILE,
        "autojoin": ["#bots"],
    }
    write_as_bot(PASSWORD_FILE, BOT["password"])
    write_as_bot(CONFIG, "\n".join([
        "[server]", *toml_lines(server), "[grants]", *toml_lines(grants),
    ]))
    return louro("connect", "--config", CONFIG)


def status_until(predicate):
    """The first status that satisfies predicate."""
    statuses = []

    def check(_last_try):
        statuses.append(louro("status"))
        return predicate(statuses[-1])

    retry(check, timeout=TIMEOUT)
    return statuses[-1]


def joined(status):
    return status["connected"] and "#bots" in status["channels"]


def start(grants):
    """Connects and waits until louro is in #bots."""
    connect(grants)
    status_until(joined)


def assert_stopped():
    """The daemon exits and leaves no socket behind."""
    machine.wait_until_fails(DAEMON_RUNNING, timeout=TIMEOUT)
    machine.fail(f"test -e {SOCKET}")


def stop():
    """What the operator does when irc.quit isn't granted."""
    machine.succeed(f"pkill -u {BOT['user']} -x louro")
    machine.wait_until_fails(DAEMON_RUNNING, timeout=TIMEOUT)
    machine.succeed(f"rm -f {SOCKET}")


machine.wait_for_unit("ergochat.service")
machine.wait_for_open_port(settings["ircPort"])
machine.wait_for_open_port(settings["apiPort"])
register(BOT["user"], BOT["password"])

with subtest("louro connects, joins #bots, and status shows its grants"):
    granted = grants()
    connected = connect(granted)
    assert connected["nick"] == BOT["user"] and connected["channels"] == ["#bots"], connected
    status = status_until(joined)
    assert status["nick"] == BOT["user"], status
    assert status["grants"] == granted, status
    stop()

with subtest("sends outside the grants, and quit without irc.quit, are refused"):
    start(grants())
    refused("not_allowed", "send", "#lounge", "hi")
    refused("not_allowed", "send", "@#bots", "hi")
    refused("not_allowed", "quit")
    assert louro("status")["connected"], "a refused quit leaves louro running"
    stop()

with subtest("with irc.quit granted, louro quit stops the daemon and removes the socket"):
    start(grants(quit=True))
    louro("quit")
    assert_stopped()
