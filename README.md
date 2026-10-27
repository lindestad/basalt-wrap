# basalt-wrap

A Rust / Ratatui dashboard and elastic VRAM supervisor for [Basalt](https://github.com/jesdga95/basalt).
Forked from [strata-wrap](https://github.com/lindestad/strata-wrap), retaining its request history, hardware panels, themes, policy controls, and process cleanup.

Requires Linux, Rust 1.88+, a systemd user manager, and one NVIDIA GPU. Basalt must already be built, with its Python environment available. The default checkout is `~/apps/basalt`.

```sh
cargo build --release --locked
./basalt-wrap --preview
./basalt-wrap /path/to/model.json
```

For Daniel's current machine, a private `local.json` and `local-model.json` are provided beside the launcher:

```sh
./basalt-wrap --config local.json
```

The model config copies the tested IQ3_S settings and enables `engine.vram-elastic` with 512 MiB segments. The wrapper targets 1152 MiB free, grows below 13952 MiB used, and waits 60 seconds of spare capacity before growing. This allows it to lend VRAM to other applications between requests. These files are ignored by Git. The source benchmark config is unchanged.

## Model configuration

The positional JSON is Basalt's model config. `--config` selects the wrapper's separate settings file.

```json
{
  "server": {
    "model": "/path/to/model.basalt",
    "alias": "local-model",
    "gpu": "0",
    "port": 8080
  },
  "engine": {
    "max-context": 262144,
    "kv": "int8",
    "vram-elastic": true,
    "vram-segment-mib": 512,
    "vram-reserve-mib": 1152
  }
}
```

Relative model paths resolve from the model config's directory. Basalt accepts underscores in option names as well as hyphens. See [examples/model.json](examples/model.json) for the IQ3_S engine settings; replace its model path before running it.

The wrapper launches `bin/basalt serve --config FILE`, forces loopback, and reads the listener port from `server.port`. `--basalt-dir`, `--gpu`, and `--port` override the corresponding selections. The GPU must be a single NVIDIA index or a UUID passed to the wrapper. Multiple engine GPUs are unsupported.

Use `--no-vram-management` to monitor a config without elastic expert caching. Otherwise `engine.vram-elastic` must be true and the wrapper's resize step must be a multiple of `engine.vram-segment-mib` (512 by default). The wrapper does not edit model configs.

Basalt refuses API keys in JSON. Set `BASALT_WRAP_API_KEY` before launch to pass a key to the server and authenticate monitoring. Use the same variable when attaching directly to a protected server.

```sh
./basalt-wrap /path/to/model.json --basalt-dir ~/apps/basalt --plain
./basalt-wrap /path/to/model.json -- --idle-unload 120
./basalt-wrap /path/to/model.json --doctor
./basalt-wrap /path/to/model.json --attach
```

The shorthand `iq3_s` selects `~/apps/basalt/basalt-iq3_s.json`; `ud-iq3_s` selects `basalt-ud-iq3_s.json` there. These optional files must already exist.

## Dashboard

The overview shows prefill/decode speed, session tokens, GPU and system memory, context occupancy, expert-cache size, MTP acceptance, conversation-cache use, and GPU processes. Request history retains the server's entries, including cache reuse, timing, and completion reason. The policy view shows memory history and the last 100 resize attempts.

Session output speed is total output tokens divided by active decode seconds. Idle and prefill time are excluded. Parallel slots contribute their individual active decode time. Incomplete slot accounting is marked in the UI.

Hardware readings use NVML and Linux process accounting. The VRAM bar includes driver-reserved memory. Process PSS, locked memory, and service-group memory overlap and should not be added together. Unavailable counters remain unavailable.

| Keys | Action |
| --- | --- |
| `1` through `4`, Tab, `h` / `l` | Overview, requests, logs, policy |
| `j` / `k`, arrows | Select requests or scroll |
| `gg` / `G`, Home / End | First / last entry |
| Ctrl+U / Ctrl+D | Half-page scroll |
| Ctrl+B / Ctrl+F, PageUp / PageDown | Page scroll |
| Enter | Request details |
| `a` | Toggle request age seconds |
| Space | Pause / resume growth; pressure shrinking stays enabled |
| `r` | Reconcile state and resume after a policy error |
| `c` | Preview policy settings from disk; Enter applies |
| `p` / Esc | Focus GPU processes / return |
| `t` | Theme picker; Enter saves, Esc cancels |
| `b` | Open Basalt's web dashboard |
| `?` / Esc | Help |
| `q`, Ctrl+C | Stop the owned server and exit; repeat to force |

An attached dashboard exits without stopping the server. `--preview` starts no server and changes no preferences. `--snapshot 100x30` renders sample data as text. Small terminals get a compact layout; redirected output uses plain summaries. `NO_COLOR` is respected.

## Wrapper settings

```sh
./basalt-wrap --init-config
./basalt-wrap --check-config
./basalt-wrap --list-gpus
./basalt-wrap --list-themes
./basalt-wrap --completions fish
```

Settings default to `~/.config/basalt-wrap/config.json`, respecting `XDG_CONFIG_HOME`. Use `--config FILE` for another file, or `--no-config` for built-in defaults. CLI options override saved settings. Relative paths in wrapper settings resolve from that file's directory.

```json
{
  "basalt_dir": "~/apps/basalt",
  "model": "/path/to/model.json",
  "vram": {
    "growth_mode": "capacity",
    "min_free_mib": 1152,
    "grow_below_mib": 13952,
    "grow_delay_seconds": 60,
    "step_mib": 512
  },
  "profiles": {
    "quiet": { "vram": { "growth_mode": "idle" } }
  }
}
```

Profiles inherit global settings; their VRAM fields override only the fields supplied. Select one with `--profile quiet`. `--setup MODEL --config FILE` creates settings for an existing checkout/model and detected GPU without overwriting files.

Automatic thresholds target at least 1 GiB free, or 1/16 of GPU memory, with another 1 GiB of hysteresis. `capacity` growth waits for sustained spare VRAM; `idle` also waits for inference to stop. Shrinking reacts to pressure in either mode. The engine applies resizes between requests. A busy engine defers the resize; uncertain errors pause management until reconciliation.

Basalt treats a reserve request as a sizing target. Actual free VRAM can remain below the requested reserve after a resize. The dashboard shows measured free VRAM and the desired reserve separately; check measured free VRAM before starting another GPU workload.

Themes: `basalt` (the inherited midnight palette), `ember`, `monochrome`, `nord`, `dracula`, `gruvbox`, `catppuccin`, `solarized-light`, `ayu-dark`, `rose-pine`, and `everforest`. `--theme` overrides saved preferences for one launch.

`install` links the current binary at `~/.local/bin/basalt-wrap`. It refuses to replace another file and does not change engine configs or shell bindings. Launch and stop remain explicit.

Logs rotate at 8 MiB with three backups under `~/.local/state/basalt-wrap`. Transient systemd units, control sockets, settings, logs, and port locks use the `basalt-wrap` namespace, separate from Strata's wrapper. A port already in use is refused.

See [AI_SETUP.md](AI_SETUP.md) for policy details and [CONTRIBUTING.md](CONTRIBUTING.md) for checks.
