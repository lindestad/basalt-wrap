# strata-wrap

A Rust / Ratatui dashboard for Strata, with model supervision and automatic VRAM management.

<img src="docs/overview.png" alt="Strata dashboard with the default theme, showing inference speed, memory, request history, and GPU processes" width="1000">

Install from crates.io:

```sh
cargo install strata-wrap --locked
strata-wrap --preview
strata-wrap /path/to/model.json --strata-dir /path/to/Strata
```

Strata must already be installed with its server environment at `.venv` inside
its checkout. The default checkout is `~/apps/Strata`; use `--strata-dir` for
another location. Requirements: Linux, Rust 1.88+, a systemd user manager, and
one NVIDIA GPU with its driver installed. The wrapper itself is entirely Rust.

For the existing `iq3_s` and `ud-iq3_s` configs under `~/apps/Strata`:

```sh
strata-wrap iq3_s
strata-wrap ud-iq3_s
```

The optional `strata-wrap install` helper enables elastic mode in both of those
configs, links the executable under `~/.local/bin`, and updates existing Fish
abbreviations `sa` and `saud` in `~/.config/fish/.config.local.fish`. It expects
both configs and abbreviations to exist. It backs up changed files under
`~/.local/state/strata-wrap/backups`. Reload an existing shell with
`source ~/.config/fish/.config.local.fish`. For other setups, use a model JSON
path and enable elastic mode in that config.

## Dashboard

- Live prefill or decode speed in the first card, switching with the current phase, plus prompt progress, elapsed time, and queued requests. Prefill shows the engine-reported rate for the current prompt.
- Session input and output tokens, prompt reuse, and average output speed. The session average is **total output tokens / active decode seconds**. Idle time and prompt processing are excluded. In-flight output is included using Strata's reported mean decode rate.
- GPU utilization, temperature, power, VRAM, PCIe traffic, CPU use, disk throughput, RAM, and swap.
- Model memory from the service's processes: proportional resident memory (PSS), anonymous/file/shared pages, locked memory, and total memory charged to the service group. Locked memory is part of resident memory; service-group accounting also includes charged file cache. These measurements overlap and should not be added together.
- Context occupancy, KV configuration, expert-cache size, resize state, MTP acceptance, and conversation-cache occupancy, restores, and evictions.
- Request history with prefill/decode rates, cache reuse, expert hits, duration, and completion reason. Prefill rates use the actual tokens read when the engine reports them, with prompt minus reused tokens as the fallback for older engines. Select a request for its timing and token details.
- A scrolling log that retains its position when new output arrives.

GPU processes include both compute and graphics workloads, sorted by VRAM usage and deduplicated by PID. The panel occupies at most one quarter of terminal height and disappears when that space cannot fit a header and three entries. Press `p` to scroll through processes beyond the visible rows. Missing driver counters are reported as unavailable. GPU readings use NVML directly. The VRAM bar counts all non-free memory, including driver-reserved memory, so used plus free equals total before display rounding. The breakdown separates Strata, other allocations, and driver-reserved memory. The growth threshold uses that same used total. RAM used is total minus available, including reclaimable memory in the available figure. Values displayed in GiB are rounded independently to one decimal place.

## Keys

| Keys | Action |
| --- | --- |
| `h` / `l`, left / right | Previous / next view |
| `1` / `2` / `3`, Tab | Overview / requests / logs |
| `j` / `k`, down / up | Select requests or scroll logs / focused GPU processes |
| `gg` / `G`, Home / End | First / last request; oldest / newest logs |
| Ctrl+U / Ctrl+D | Half-page up / down |
| Ctrl+B / Ctrl+F, PageUp / PageDown | Page up / down |
| Enter | Open the selected request's details |
| `p` / Esc | Focus GPU processes / return |
| `t` | Open the theme picker; `hjkl` previews, Enter applies, Esc cancels |
| `b` | Open Strata's web dashboard |
| `?` / Esc | Open / close help |
| `q`, Ctrl+C | Stop Strata and exit; repeat to force |

The layout adapts to terminal size, with a compact summary below 50 columns or 14 rows. `NO_COLOR` is respected. Redirected output uses plain summaries automatically; `--plain` selects those explicitly and `--raw` includes unfiltered server output in plain mode.

```sh
strata-wrap --demo                      # Preview without starting a model
strata-wrap /path/to/model.json         # Another elastic Strata config
strata-wrap iq3_s --plain
strata-wrap iq3_s -- --idle-unload 120   # Forward Strata server options after --
```

The server stays on loopback, with the API key read from its model config. The existing web dashboard remains available on port 8080; `--port` changes both the listener and monitoring address. Logs rotate at 8 MiB with three backups under `~/.local/state/strata-wrap/`. Strata's native engine log remains at the path in its config.

## Themes

The original palette, `strata`, remains the default. Press `t` to preview themes
on the dashboard. Move with `hjkl`, arrow keys, or Tab. Enter saves the selection
for future launches; Esc restores the previous palette. This also works while
the model is generating.

<img src="docs/themes.png" alt="Six theme previews: Ember, Monochrome, Ayu Dark, Rosé Pine, Everforest, and Solarized Light" width="1000">

| Theme | Appearance |
| --- | --- |
| `strata` | Midnight blue with sea glass and lavender |
| `ember` | Black and white with burnt orange accents and graphs |
| `monochrome` | Grayscale throughout, including status indicators and graphs |
| `nord` | Polar slate with frost blue and muted aurora colors |
| `dracula` | Charcoal violet with bright pastel accents |
| `gruvbox` | Warm charcoal with earthy gold and aqua |
| `catppuccin` | Mocha with soft teal and mauve |
| `solarized-light` | Warm ivory with blue and violet accents |
| `ayu-dark` | Ink black with amber and sky blue |
| `rose-pine` | Dusk violet with rose and iris |
| `everforest` | Forest charcoal with sage and sea green |

```sh
strata-wrap iq3_s --theme ember         # Override the saved theme for this launch
strata-wrap --list-themes
strata-wrap --preview --theme ember     # Run alongside an existing session
```

`--preview` (also available as `--demo`) uses sample data. It starts no server,
opens no listener, and makes no systemd, VRAM, or preference changes. Press `t`
to try palettes and `q` to close it. The running model continues independently.
You can also launch `./strata-wrap --preview` from a separately built worktree.

Normal sessions save confirmed selections in the selected wrapper config, which
defaults to `~/.config/strata-wrap/config.json` (under `$XDG_CONFIG_HOME` when set).
`--theme` takes precedence without saving; selecting a theme in the picker saves
it to the wrapper config. Existing `preferences.json` themes remain a fallback
until a theme is saved in the new config. A running session keeps its own palette
when another session changes the saved choice. Snapshots use the configured
theme, or `strata` when none is configured. Pass `--theme` to render another
palette.

The inspired palettes adapt [Nord](https://www.nordtheme.com/docs/colors-and-palettes/),
[Dracula](https://draculatheme.com/contribute),
[Gruvbox](https://github.com/morhetz/gruvbox),
[Catppuccin Mocha](https://github.com/catppuccin/catppuccin),
[Solarized](https://ethanschoonover.com/solarized/),
[Ayu Dark](https://github.com/Shatur/neovim-ayu),
[Rosé Pine](https://github.com/rose-pine/neovim), and
[Everforest](https://github.com/sainnhe/everforest) for this dashboard.

## User configuration

Create and validate a per-user config:

```sh
strata-wrap --init-config
strata-wrap --check-config
strata-wrap --list-gpus
```

Settings live at `$XDG_CONFIG_HOME/strata-wrap/config.json`, or
`~/.config/strata-wrap/config.json`. Use `--config /path/to/config.json` for
another profile and `--no-config` to use built-in defaults. CLI options override
saved settings. Model JSON files remain separate: the positional model selects
Strata's own config, while `--config` selects the wrapper's settings.

```json
{
  "schema_version": 1,
  "gpu": 0,
  "vram": {
    "growth_mode": "capacity",
    "min_free_mib": 1024,
    "grow_below_mib": 14336,
    "grow_delay_seconds": 60,
    "poll_seconds": 2,
    "step_mib": 512,
    "accounting_margin_mib": 16,
    "no_op_retry_seconds": 60
  }
}
```

This example targets a 16 GiB card. Leave the thresholds `null` or omit them to
size them from the selected GPU's measured memory. Defaults reserve at least
1 GiB, or 1/16 of VRAM on larger cards, and leave another 1 GiB of hysteresis
before allowing growth. A 12 GiB card and a 32 GiB card therefore get different
thresholds. See [AI_SETUP.md](AI_SETUP.md) for recommendations and the full schema.

The `gpu` setting accepts an NVIDIA index or UUID. The wrapper passes the same
selection to Strata and NVML, so monitoring and resize decisions follow the
engine's card. Elastic management supports one NVIDIA GPU per model; layer
splits and peer-GPU expert caches are unsupported.

## VRAM policy

The supervisor requests a cache reduction when free VRAM drops below the
reserve. Growth requires usage below the growth threshold for the configured
delay, enough room for a step while preserving the reserve, and room below the
engine's full cache size. The threshold is a trigger, so one step may take usage
above it. Driver-reserved memory counts as used.

`idle` mode, the default, requires a continuous period without active or queued
inference. `capacity` mode counts sustained spare VRAM while inference continues
and queues a resize for Strata to apply between requests. Each completed resize
starts a fresh growth period. Crossing the threshold, changed cache capacity,
or interrupted telemetry resets it. This prevents consecutive grows from using
one allowance. A busy session can keep `idle` mode from growing indefinitely.

Strata accepts a free-memory target, so step size is approximate: segment
rounding and allocations made while a resize waits can change the result.
Only one resize is outstanding. No-op and engine-busy responses get a cooldown
before retrying. Other failed or uncertain resizes pause management and report
why; inference continues. Set `vram.enabled` to `false` to monitor without
requesting resizes. A reserve cannot guarantee space for a sudden allocation.

To inspect a running server without starting or resizing it:

```sh
strata-wrap iq3_s --diagnose
```

The report includes measured GPU memory, selected policy, live cache size,
inference activity, and growth blockers. It does not include prompts, responses,
or the API key. The TUI shows the running worker's countdown and pending state.

## Shutdown

Every launch owns a transient `strata-wrap.service` user service. It starts on demand, without a restart loop or boot activation. Duplicate launches are refused. A port lock also prevents separate wrapper units from managing the same server.

```sh
systemctl --user stop strata-wrap
```

Shutdown sends SIGTERM to Strata for its normal cleanup. An unresponsive server is killed after 12 seconds. systemd's `KillMode=mixed` and 20-second stop timeout contain and clean up all descendants, including children that create separate process groups or sessions. Killing the service's main wrapper also cleans up the engine. The worker watches the foreground launcher's pidfd, so killing that launcher triggers shutdown too. Terminal state is restored on normal exit, errors, handled signals, and panics.

## Setup and development

See [AI_SETUP.md](AI_SETUP.md) for agent-assisted setup and card-specific settings.
Build, test, and release instructions are in [CONTRIBUTING.md](CONTRIBUTING.md).
Version notes are in [CHANGELOG.md](CHANGELOG.md).

## License

MIT. See [LICENSE](LICENSE).
