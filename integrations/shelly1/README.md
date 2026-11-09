# Shelly Gen1 (`shelly1`)

First-generation Shelly devices (SHSW-1, SHSW-25, SHSW-PM, SHSW-01PM…)
controlled over their plain HTTP API — **not** the Gen2+ websocket
JSON-RPC that the `shelly` driver speaks.

## Points

| Point | Relay | Access |
|---|---|---|
| `state` (`state_2`, `state_3`… on multi-relay models) | ON/OFF | read + write |
| `power`, `energy`, `voltage`, `current` (same suffixes) | only on PM models | read-only |
| `model` | device type from `/shelly` | read-only |

Commands go to `GET /relay/<n>?turn=on|off|toggle`; state is polled
with `GET /status` every `poll_secs` (default 30 s). `GET /shelly`
and `GET /settings` provide the model and hostname.

## Configuration

```toml
[[driver]]
id = "shelly"
kind = "shelly1"

[driver.options]
devices = ["192.168.1.x"]
poll_secs = 30
```

Authentication is not wired yet: devices must have it off (the
default) or the driver cannot reach them.
