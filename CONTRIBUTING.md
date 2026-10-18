# Contributing

## Development

```sh
cargo build --release --locked
./strata-wrap --preview
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

Tests cover weighted session throughput, navigation, scrolling, resize timing in idle and capacity modes, bounded retries, per-user config validation and overrides, worker settings forwarding, all palettes and overlays at small terminal sizes, grayscale output, theme selection and persistence, preview isolation, and the process-panel height limit. Integration tests use the real systemd user manager and a Rust HTTP fixture whose child escapes into a new session and ignores SIGTERM. They exercise interrupt, hangup, termination, launcher and worker SIGKILL, service stop, server crashes, duplicate launches, and installation. The ignored `fixture_process` test is the child fixture entry point, not a skipped check. Integration tests use port 18089 and remove their transient services afterward.

## Releases

CI checks formatting, Clippy, tests, crate packaging, and Rust 1.88 compatibility.
The Linux runner starts a systemd user manager for the lifecycle tests; it does
not need an NVIDIA GPU.

Version tags such as `v0.3.0` verify the matching Cargo version, publish to
crates.io through GitHub OIDC Trusted Publishing, and attach a Linux x86_64
binary to a GitHub release. The crates.io publisher is restricted to
`lindestad/strata-wrap`, `.github/workflows/release.yml`, and the `crates-io`
environment. The first crate version is published manually before configuring
Trusted Publishing. A release skips crate upload when that version is already
published, so the bootstrap release can still build its binary.
