# Scripting louro

Anything with a shell can drive louro, not only an agent. Every command
prints JSON on stdout, errors included, and exits with a fixed code.

## An echo bot

This loop echoes back every DM from a person (not a bot), using
[jq](https://jqlang.org). It needs `irc.dm.read` listing who may DM it, and
`irc.dm.send = ["$first"]` to answer them:

```bash
#!/usr/bin/env bash

reply() {
  until louro send "$1" "$2" >/dev/null; do
    case $? in
      4|6) sleep 5 ;;  # IRC is down, or rate limited: try again
      3) exit 1 ;;     # louro isn't running
      *) return ;;     # 5 is not granted or blocked: don't retry
    esac
  done
}

while true; do
  batch=$(louro wait --timeout 300)
  case $? in
    0) ;;
    3) echo "louro isn't running" >&2; exit 1 ;;
    *) sleep 5; continue ;;
  esac
  while read -r dm; do
    reply "$(jq -r .from.nick <<<"$dm")" "you said: $(jq -r .text <<<"$dm")"
  done < <(jq -c '.events[] | select(.type == "message" and .private and (.from.bot | not))' <<<"$batch")
done
```

Each `wait` takes the next batch, so the loop never sees an event twice.

## Exit codes

An error prints `{"error": {"code", "message", "hint"?, "retry_after"?}}`
and one plain line on stderr.

| Exit | Codes | Means |
|---|---|---|
| 0 | | it worked |
| 1 | `internal` | a bug in louro |
| 2 | `usage`, `too_long` | bad arguments, or the text is too long |
| 3 | `no_session`, `session_exists` | louro isn't running, or already is |
| 4 | `not_connected` | louro is up, but its IRC link is down |
| 5 | `not_allowed`, `blocked`, `blocked_secret` | not granted, the filter blocked it, or it contains the password |
| 6 | `rate_limited` | over a [limit](configuration.md#limits); see `retry_after` |
| 7 | `unsupported` | the server can't do that |
| 8 | `filter_failed` | the [filter](configuration.md#filter) failed |
| 9 | `auth_failed`, `connect_failed`, `tls` | `connect` couldn't log in or reach the server |
| 10 | `server_error`, `request_timeout` | the server refused, or didn't answer |
| 11 | `version_mismatch` | the CLI and the daemon are different versions; the hint says which is stale |
