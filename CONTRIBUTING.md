# Contributing

Keep Basalt's launch/config boundary separate from the dashboard and memory policy. The wrapper targets Linux and supervises one NVIDIA GPU through a transient systemd user service.

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
```

Lifecycle tests use a real systemd user manager and a fake engine. They exercise launcher signals, worker death, stubborn descendants, crashes, duplicate launches, attachment, and installation without loading model weights. They bind port 18089 and run their cases sequentially.

For engine integration, use Basalt's mock server first. A real model smoke should verify status, metrics, a completion, and an expert-cache shrink/grow. Coordinate GPU ownership before running it.

Preview rendering must work without Basalt, NVML, or a systemd service. Avoid saving preferences or opening listeners from preview mode. Preserve unrelated settings when saving a theme.

This repository derives from strata-wrap 0.3.0 (838b357). Keep its MIT license and attribution when redistributing it.
