# Setting up strata-wrap for a user

Use this guide when a user asks an assistant to install or configure strata-wrap.
It launches and supervises one Strata server on Linux and monitors one NVIDIA GPU.
Strata itself, its Python environment, engine, and models must already be
installed. For that part, follow Strata's own
[AI setup guide](https://github.com/Niko1221/Strata/blob/main/docs/AI_SETUP.md).

## Inspect the existing setup

Read the user's existing config before editing it. Preserve model paths, API
keys, engine arguments, and unrelated settings. Keep wrapper settings separate
from Strata model JSON. Do not print model config contents or credentials into
logs, screenshots, commits, or chat.

Check the environment:

```sh
uname -s
systemctl --user show-environment
nvidia-smi --query-gpu=index,uuid,name,memory.total,memory.used,memory.free,memory.reserved --format=csv
free -h
strata-wrap --list-gpus
```

Use the measured total VRAM, not the card's advertised capacity. NVIDIA indices
can change; a GPU UUID is a useful persistent selection. Verify which GPU the
model uses. One card at a time is supported. Do not present layer splitting,
peer expert caches, AMD, Windows, or macOS as supported by this wrapper.

Find the Strata checkout and the model JSON files. Confirm that
`<checkout>/.venv/bin/python` and `<checkout>/serve/server.py` exist. Check for an
already running server or `strata-wrap.service` before attempting to start one.
Use `--preview` for UI checks while a user's session is running, or `--attach`
for a read-only dashboard of the existing worker/server. Quitting an attachment
must leave the server running.

## Install the wrapper

```sh
cargo install strata-wrap --locked
strata-wrap --preview
```

Linux, a working systemd user manager, and NVIDIA's NVML library are required for
normal operation. Do not install drivers or change system services as part of a
routine wrapper setup. If privileged changes are necessary, explain them and
ask the user to run the commands.

The optional `strata-wrap install` helper targets an existing personal layout:
both `~/apps/Strata/strata-iq3_s.json` and `strata-ud-iq3_s.json`, plus existing
`sa` and `saud` abbreviations in `~/.config/fish/.config.local.fish`. Use it only
when those files and abbreviations exist and the user wants those changes. It
backs up changed files. Other users can run a model JSON path directly and
choose their own shell bindings.

## Choose the policy with the user

Ask about the workload: a dedicated inference card, a desktop card, gaming or
other bursty GPU applications, and whether inference is nearly continuous.

- `idle` growth requires a continuous interval without active or queued
  inference. Choose it when reclaiming VRAM should happen during idle periods.
- `capacity` growth requires sustained spare VRAM but allows inference to
  continue. Strata applies the queued resize between requests. Choose it for
  continuous workloads where an idle interval may never occur.
- Start with 60 seconds per growth step and 2-second monitoring. Shorter growth
  delays can cause repeated grow/shrink cycles with bursty applications.
- Start with 512 MiB steps. The step must be a multiple of the model's segment
  size; the engine requires segments of at least 64 MiB. Keep the default 16 MiB
  accounting margin unless measurements justify another value.

The automatic reserve is `max(1024, total_mib / 16)` MiB. The automatic growth
trigger is `total_mib - reserve_mib - 1024` MiB. Omitted or `null` thresholds
enable automatic sizing. These nominal examples illustrate the policy; the
actual defaults use the measured total:

| Nominal VRAM | Starting reserve | Automatic growth trigger | Notes |
| --- | --- | --- | --- |
| 12 GiB | 1 GiB | about 10 GiB used | Smaller models or context may be needed |
| 16 GiB | 1 GiB | about 14 GiB used | Consider a larger reserve for desktop bursts |
| 24 GiB | 1.5 GiB | about 21.5 GiB used | Account for other GPU applications |
| 32 GiB, such as an RTX 5090 | 2 GiB | about 29 GiB used | Capacity mode suits continuous inference |

Increase the reserve for measured application bursts, larger contexts, or
vision. A larger card does not require a different code path. A smaller card
may need a different Strata model or context configuration; changing wrapper
thresholds cannot make an oversized model fit.

The growth trigger is not a hard cap: a step can cross it. Growth must still
preserve the configured reserve. Usage includes driver-reserved memory.
Displayed GiB values are rounded, while policy decisions use MiB.

## Create the user's config

```sh
strata-wrap --init-config
```

For an existing checkout/model, `strata-wrap /path/to/model.json --strata-dir
/path/to/Strata --setup` creates a starter config using the selected GPU UUID.
It refuses to overwrite files and leaves model JSON and shell bindings alone.
Run `--doctor` afterward. Setup does not install Strata or enable elastic mode.

```sh
strata-wrap /path/to/model.json --doctor
```

This creates `$XDG_CONFIG_HOME/strata-wrap/config.json`, or
`~/.config/strata-wrap/config.json`. It refuses to overwrite an existing file.
Edit that file, preserving existing values. Use `--config PATH` for another
profile. Relative paths in that file resolve against its directory; `~/` is
expanded. CLI paths resolve from the current working directory.

A minimal config with automatic sizing:

```json
{
  "schema_version": 1,
  "strata_dir": "~/apps/Strata",
  "model": "iq3_s",
  "gpu": 0,
  "vram": {
    "growth_mode": "capacity"
  }
}
```

Replace `gpu` with the correct index or UUID. Set `model` to an existing Strata
model JSON path when the built-in name is inappropriate. With a default model,
`strata-wrap` alone launches it. CLI options take precedence over saved values.

The complete config schema has these fields:

| Field | Default | Meaning |
| --- | --- | --- |
| `age_seconds` | `false` | Show seconds in request ages; `a` toggles this for the current dashboard |
| `profiles` | `{}` | Named setting overrides; select with `--profile NAME` |
| `schema_version` | `1` | Config format version |
| `strata_dir` | `null` | Strata checkout; defaults to `~/apps/Strata` |
| `model` | `null` | Default model name or model JSON path |
| `port` | `null` | Override model port |
| `unit` | `"strata-wrap"` | systemd user unit name |
| `gpu` | `null` | NVIDIA index or UUID; falls back to the model's GPU, then index 0 |
| `theme` | `null` | Theme name; an old `preferences.json` remains a fallback |
| `vram.enabled` | `true` | Allow automatic resizing |
| `vram.min_free_mib` | `null` | Minimum reserve, or automatic sizing |
| `vram.grow_below_mib` | `null` | Grow below this usage, or automatic sizing |
| `vram.grow_delay_seconds` | `60` | Sustained qualifying time for each grow |
| `vram.poll_seconds` | `2` | Hardware sampling and shrink cooldown |
| `vram.growth_mode` | `"idle"` | `idle` or `capacity` |
| `vram.step_mib` | `512` | Approximate cache change per resize |
| `vram.accounting_margin_mib` | `16` | NVML/CUDA accounting allowance |
| `vram.no_op_retry_seconds` | `60` | Retry cooldown after no-op or engine-busy responses |

Unknown fields and invalid values are errors. `--no-config` bypasses the user
config. Copyable profiles are in [examples](examples/).

In Strata's model JSON, enable `vram_elastic` and use a compatible
`vram_segment_mib` (normally 512). Preserve the API key and loopback binding.
Do not put the API key in the wrapper config or forward it on the command line.

## Validate and start

```sh
strata-wrap --check-config
strata-wrap --preview
strata-wrap /path/to/model.json --doctor
strata-wrap /path/to/model.json --strata-dir /path/to/Strata
```

Check that the selected GPU, reserve, growth trigger, step size, and mode match
the user's intended policy. Initial model loading can be lengthy. When a
server is already running under the wrapper, leave it running and use:

```sh
strata-wrap /path/to/model.json --diagnose
```

For a current worker, this reads its private status socket and reports the
settings actually running, policy state, pending resize, history, and telemetry
age. No request content or key is printed. Older wrappers lack that socket;
the fallback is clearly labeled as server counters plus current file settings.
Do not describe those settings as the old worker's effective policy.

The Policy view (`4`) shows resize results and memory graphs. Space pauses
only growth, keeping pressure shrinking enabled. `r` reconciles fresh engine
and GPU state before resuming after an error; it refuses while a resize is
pending. `c` previews VRAM settings from disk before Enter applies them. Reload
replaces startup policy overrides and resets growth timing; GPU/model/port/unit
changes still require a new launch. An attachment cannot use these controls.

Named profiles inherit root settings and override individual VRAM fields.
Use `--list-profiles` to inspect names and `--profile NAME --doctor` before
starting a profile. A theme chosen in a profile is saved to that profile.

After startup, send a small request through the user's existing client and
check token output. Verify normal shutdown when convenient for the user:

```sh
systemctl --user stop strata-wrap
```

The service contains the engine and its descendants. A second wrapper must not
be started against the same port. A stopped or killed wrapper must not leave
an engine running.

## Trace a resize problem

1. Confirm which executable is running and which config it loaded. Startup
   logs show GPU selection and the effective VRAM policy. A running worker
   keeps the settings it received at launch; editing a file takes effect on
   the next launch unless its VRAM settings are previewed and reloaded.
2. Use `--diagnose` and the TUI. Growth can be blocked by the usage threshold,
   reserve, full cache, active inference in idle mode, or interrupted telemetry.
3. Inspect wrapper resize lines under `~/.local/state/strata-wrap/` (or
   `$XDG_STATE_HOME/strata-wrap/`). Logs also contain native server output, so
   avoid dumping them into public issues or commits.
4. Compare cache sizes before and after the response. A free-memory target can
   yield a different step because of segment rounding or allocations made while
   it waits. A requested grow can even become a shrink if pressure changes.
5. A no-op is retried after a cooldown. `model_busy` and the engine's batch-decoding busy responses are deferred and
   retried. Other failed or uncertain responses pause resizing to avoid acting
   on an unknown cache state; report the error before considering a restart.

Do not diagnose every low-utilization reading as a wrapper or engine defect.
Temporary inference allocations can change total VRAM use without changing
the expert cache. Verify the cache counters and control responses first.
