# Contributing

## Development

```sh
cargo build --release --locked
./strata-wrap --preview
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

Tests cover weighted session throughput, navigation, scrolling, resize timing in idle and capacity modes, bounded retries, per-user config validation and overrides, worker settings forwarding, all palettes and overlays at small terminal sizes, grayscale output, theme selection and persistence, preview isolation, and the process-panel height limit. A deterministic controller scenario drives busy inference, stale metrics, a queued grow, pressure, and a confirmed shrink through the same eligibility code and HTTP client as the worker. Socket tests cover live state, controls, permissions, and cleanup. Integration tests use the real systemd user manager and a Rust HTTP fixture whose child escapes into a new session and ignores SIGTERM. They exercise interrupt, hangup, termination, launcher and worker SIGKILL, service stop, server crashes, duplicate launches, attachment shutdown without stopping the engine, live-worker diagnostics, and installation. The ignored `fixture_process` test is the child fixture entry point, not a skipped check. Integration tests use port 18089 and remove their transient services afterward.

For a native smoke run, stop any existing engine first and use a separate
unit/port, such as `--unit strata-wrap-smoke --port 18090`. Verify generation,
a confirmed grow during sustained inference, growth pause, the reload preview,
and recovery. Open a read-only attachment and check that closing it leaves the
worker alive. Finally stop the test unit and verify its engine processes and
status socket are gone. Keep API keys and native logs out of commits.

## Releases

CI checks formatting, Clippy, tests, crate packaging, and Rust 1.88 compatibility.
The Linux runner starts a systemd user manager for the lifecycle tests; it does
not need an NVIDIA GPU.

Version tags such as `v0.3.0` verify the matching Cargo version, publish to
crates.io through GitHub OIDC Trusted Publishing, and attach a Linux x86_64
binary to a GitHub release. Binary archives include setup/development docs,
examples, screenshots, and a SHA-256 checksum. Tags independently run formatting,
Clippy, tests, packaging, and the MSRV check before publishing. The crates.io publisher is restricted to
`lindestad/strata-wrap`, `.github/workflows/release.yml`, and the `crates-io`
environment. The first crate version is published manually before configuring
Trusted Publishing. A release skips crate upload when that version is already
published, so the bootstrap release can still build its binary.
