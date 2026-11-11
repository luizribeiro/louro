# Filtering what the bot says

A filter is a program of yours that sees everything the bot would show
others before it reaches IRC. It can let the text through, rewrite it, or
block it. Use it to catch what the agent shouldn't paste into a chat:
tokens, keys, internal hostnames.

louro already refuses any text that contains the bot's own password. The
filter is for everything else you care about.

## An example: redact tokens, block private keys

Save this as `/usr/local/bin/louro-redact`:

```bash
#!/usr/bin/env bash
set -euo pipefail

text=$(cat)

if grep -q -- '-----BEGIN [A-Z ]*PRIVATE KEY-----' <<<"$text"; then
  echo "that's a private key; describe it instead of pasting it" >&2
  exit 10
fi

redacted=$(sed -E \
  -e 's/gh[pousr]_[A-Za-z0-9]{36,}/[redacted]/g' \
  -e 's/AKIA[0-9A-Z]{16}/[redacted]/g' \
  -e 's/(^|[^A-Za-z0-9_])sk-[A-Za-z0-9_-]{20,}/\1[redacted]/g' \
  <<<"$text")

if [[ $redacted != "$text" ]]; then
  echo "redacted a token" >&2
fi
printf '%s\n' "$redacted"
```

It blocks private keys, and replaces GitHub tokens, AWS access key IDs and
`sk-` API keys with `[redacted]`. Make it executable and try it by hand:

```
$ chmod +x /usr/local/bin/louro-redact
$ echo "push with ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" | louro-redact
redacted a token
push with [redacted]
$ printf -- '-----BEGIN OPENSSH PRIVATE KEY-----\n...' | louro-redact; echo "exit $?"
that's a private key; describe it instead of pasting it
exit 10
```

Then add it to `louro.toml` and restart louro (`louro connect --replace`):

```toml
[filter]
outgoing = ["/usr/local/bin/louro-redact"]
# timeout = "5s"
```

`connect` refuses to start if the program isn't an executable file.

## What the agent sees

When the filter rewrites a send, it goes out rewritten, and the result says so:

```json
{"sent": true, "target": "#bots", "filtered": true, "text": "push with [redacted]", "note": "redacted a token", …}
```

When it blocks one, nothing is sent and `louro send` exits 5:

```json
{"error": {"code": "blocked", "message": "the operator's filter blocked this text, so it was not sent", "note": "that's a private key; describe it instead of pasting it"}}
```

The filter's stderr becomes the `note`, so use it to tell the agent what to
do instead. louro also logs every rewrite, block and failure on its own
stderr (the journal, under systemd).

## How a filter works

- It gets the text on stdin.
- Exit 0 sends what it prints, with one trailing newline trimmed. The result is checked
  again like the agent's own text (length, the password).
- Exit 10 blocks the text.
- Anything else sends nothing and tells the agent to ask you (`filter_failed`, exit 8):
  another exit code, running past the timeout, or output that is empty, not UTF-8 or over
  64 KiB.
- A short stderr reaches the agent as a `note`, unless the filter failed.
- It runs as louro's user, in `/`. Under the [NixOS service](running.md#as-a-nixos-service)
  that's the service user, inside its systemd sandbox.

It runs on every text the bot would show others, not only messages. These
env vars say what the text is, each unset when it doesn't apply:

| Variable | What it is |
|---|---|
| `LOURO_KIND` | `message`, `notice`, `action`, `topic`, `part`, `redact`, `react`, `unreact`, `nick`, `away`, `realname` or `ask` |
| `LOURO_CHANNEL` | the channel it's for |
| `LOURO_NICK`, `LOURO_ACCOUNT` | the person it's for |
| `LOURO_MSGID` | the message it reacts to, replies to or redacts |
| `LOURO_SELF_NICK`, `LOURO_SELF_ACCOUNT` | the bot |
| `LOURO_SERVER` | the server's address |
| `LOURO_FILTER_VERSION` | `1` |

For example, to leave reactions alone:

```bash
case ${LOURO_KIND:-} in
  react|unreact) cat; exit 0 ;;
esac
```

`louro ask` sends a question only as written: if the filter would change
it, it isn't asked at all.

## Limits

A filter is a pattern check, not a guarantee. An agent can spell a secret
out, encode it, or split it across messages, and no regex catches every
form. Keep real secrets out of the agent's reach first; see
[security](security.md).
