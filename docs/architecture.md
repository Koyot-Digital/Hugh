# Architecture and extension guide

Hugh is intentionally a small service with directional dependencies:

```text
Discord gateway -> bot event adapter -> stateful policy -> pure detectors
                         |                    |
                         +-> enforcement     +-> bounded in-memory windows
                         +-> alerts
                         +-> JSONL incidents
```

- `main.rs` owns startup, logging, signals, and Discord intents.
- `config.rs` parses and strictly validates the operator's policy.
- `detection.rs` contains pure, side-effect-free content and role checks.
- `state.rs` owns rolling windows and turns observations into decisions.
- `bot.rs` adapts Serenity events and performs Discord API actions.
- `incident.rs` appends privacy-minimal, machine-readable incident records.

Detector code does not call Discord. This keeps threshold behavior cheap to
test and lets future frontends reuse the same policy. Discord API failures never
erase the incident: Hugh records an explicit failure outcome and logs the error.

## Adding a protection

1. Add a strict configuration section and validation in `config.rs`.
2. Put stateless parsing in `detection.rs`, or bounded rolling data in
   `state.rs`.
3. Return a named violation/decision rather than enforcing inside the detector.
4. Wire the smallest suitable gateway event in `bot.rs`.
5. Record what was detected and whether enforcement succeeded, without storing
   message content or secrets.
6. Add boundary tests: exactly at the limit, one over it, expiry, trusted
   bypass, and disabled behavior.
7. Document permissions, false-positive risks, and recovery.

State is intentionally local and ephemeral. This is correct for a single bot
process protecting one community. A multi-process deployment would need a
shared atomic window store and coordinated enforcement; running two Hugh
instances against the same guild is unsupported.

## Performance characteristics

Message processing is linear in the small number of active timestamps for that
user. Old entries are pruned and inactive users are periodically evicted.
Duplicate message bodies are hashed and discarded. Disk and Discord I/O only
happen after a violation. The JSONL sink is serialized to preserve complete
lines.
