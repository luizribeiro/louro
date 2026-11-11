# louro ask

`louro ask` DMs someone a question and blocks until they answer. It's made
for hooks, say to ask you before an agent runs a tool. It's experimental:
its flags, result and the DM it sends may still change.

```
louro ask NICK TEXT [--choices yes,no] [--timeout 5m]
```

Those are the defaults. louro DMs `[ask k3fa] TEXT` and
`reply !yes k3fa or !no k3fa`, then prints:

```json
{"id": "k3fa", "target": "luiz", "answer": "yes", "timed_out": false}
```

- Once the timeout (at most 1h) passes, `answer` is `null` and `timed_out` is `true`.
  Both exit 0.
- If louro stops meanwhile, it exits 3 (`no_session`).
- Answer with `!yes k3fa`. A bare `!yes` works when it's the only pending ask to you and
  none closed in the last 10s; otherwise the bot replies "which ask?".
- Answers never reach `wait`. If `irc.react` lists you, the bot reacts 🫡 once your
  answer is recorded.

## Grants and safety

- The nick must pass `irc.dm.send`, and `irc.ask` must list its `account:NAME`.
- An answer counts only from a live DM whose account matches.
- The agent could post a look-alike question for your answer to land on, so it can't send
  or react to you while an ask to you is pending.
- The question goes out whole or not at all: too long is refused, and so is anything the
  filter would change.
