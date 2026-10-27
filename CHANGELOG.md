# Changelog

## 0.1.0

- Fork strata-wrap 0.3.0 for Basalt's `bin/basalt serve` launcher and nested model configuration.
- Retain the dashboard, request history, themes, elastic VRAM policy, live controls, and process cleanup.
- Use separate settings, logs, control sockets, and systemd units.
- Read API credentials from `BASALT_WRAP_API_KEY` for server launch and monitoring.
- Sum session tokens and decode time across polls: Basalt lists only its recent requests and gives a decode rate instead of a decode duration.
- Poll Basalt's `/slots` table so the `Now` panel's progress bar fills through prefill and decode instead of only counting elapsed time.
- Read the conversation cache budget as GiB and show the quantization on its own line; the values were clipped or mis-scaled at typical panel widths.
- Count the session from the server's start, not the wrapper's, so an attachment shows the session it joined; follow the running server's log from an attachment, over the worker's file or the server's `/api/logs`.
- Show the session's token cost in the header, at the model config's `pricing` rates: cached prompt tokens at the cache-read rate, the rest at prompt and completion rates. A `≥` marks the figure as a floor when history is incomplete.
- Make installation link only the launcher, preserving model configs and shell bindings.
