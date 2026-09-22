# Changelog

All notable changes will be documented here. This project follows Semantic
Versioning and the Keep a Changelog format.

## [Unreleased]

### Added

- Webhook incident delivery with a local JSONL fallback.
- `/lock`, `/banish`, `/unbanish`, `/help`, and `/hugh` guild commands.
- Durable quarantine role snapshots and automatic channel isolation.
- A project introduction when Hugh is mentioned.

## [0.1.0] - 2026-09-22

### Added

- Single-guild event scoping and strict TOML configuration validation.
- Join-raid detection with young-account log, timeout, or kick actions.
- Mass mention, protected staff-role ping, flood, duplicate, and invite guards.
- Explicit-allowlist protected-role enforcement against self-elevation.
- Private Discord alerts and privacy-minimal append-only JSONL incidents.
- Graceful shutdown, structured logging, tests, Docker packaging, and guides.
