# Contributing to Hugh

Thank you for helping protect communities. Keep changes focused and open an
issue before large architectural work.

## Development

Install stable Rust, fork the repository, and run:

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Never use a production community for development. Use a private test server and
a separate bot token. Do not commit `config.toml`, `.env`, incident data, tokens,
or copied user message content.

Pull requests should include tests for policy changes, documentation for new
configuration, a clear false-positive analysis, and any newly required Discord
permissions or intents. Keep detectors independent of Discord API calls where
possible. Dependencies need a concrete benefit and must use a maintained,
compatible license.

By contributing, you agree that your work is licensed under this repository's
MIT License.
