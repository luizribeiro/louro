# Helpers for Ergo and the people on IRC that irc.nix sets up, shared by the VM
# tests; each test's script runs after this one, with `settings` from its .nix.
import datetime as dt
import json
import shlex

TIMEOUT = dt.timedelta(minutes=2)


def ergo_api(endpoint, account, password):
    request = json.dumps({"accountName": account, "passphrase": password})
    response = machine.succeed(shlex.join([
        "curl", "-sf", "-H", f"Authorization: Bearer {settings['apiToken']}",
        "-d", request, f"http://127.0.0.1:{settings['apiPort']}/v1/{endpoint}",
    ]))
    assert json.loads(response)["success"], response


def start_ergo(accounts):
    """Waits for Ergo and registers each account (nick: password)."""
    machine.wait_for_unit("ergochat.service")
    machine.wait_for_open_port(settings["ircPort"])
    machine.wait_for_open_port(settings["apiPort"])
    for account, password in accounts.items():
        ergo_api("saregister", account, password)


def until(attempt, predicate):
    """Retries attempt() until predicate holds for what it returned; returns that."""
    results = []

    def check(_last_try):
        results.append(attempt())
        return predicate(results[-1])

    retry(check, timeout=TIMEOUT)
    return results[-1]


def human_file(nick, name):
    """irc-human's FIFO ("in") or log, as irc.nix's service sets them."""
    return f"/run/irc-human-{nick}/{name}"


def send_raw(nick, line):
    machine.succeed(f"echo {shlex.quote(line)} > {human_file(nick, 'in')}")


def split_tags(line):
    """An IRC line's message tags as a dict, and the rest of the line."""
    if not line.startswith("@"):
        return {}, line
    tags, rest = line[1:].split(" ", 1)
    return dict((tag.split("=", 1) + [""])[:2] for tag in tags.split(";")), rest


def received(nick, matches):
    """Waits until nick receives a line whose tags and rest satisfy matches; returns its tags."""

    def found():
        log = machine.succeed(f"cat {human_file(nick, 'log')}").splitlines()
        return [tags for tags, rest in map(split_tags, log) if matches(tags, rest)]

    return until(found, bool)[0]
