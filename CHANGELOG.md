# Changelog

## 0.1.0

- Fork strata-wrap 0.3.0 for Basalt's `bin/basalt serve` launcher and nested model configuration.
- Retain the dashboard, request history, themes, elastic VRAM policy, live controls, and process cleanup.
- Use separate settings, logs, control sockets, and systemd units.
- Read API credentials from `BASALT_WRAP_API_KEY` for server launch and monitoring.
- Make installation link only the launcher, preserving model configs and shell bindings.
