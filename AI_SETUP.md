# Policy setup

Build Basalt and install its Python dependencies before starting the wrapper. `--doctor MODEL` checks the checkout, executable, Python, model file, GPU, systemd user manager, port, and policy compatibility without loading weights.

The model JSON uses Basalt's `server` and `engine` blocks. Enable `engine.vram-elastic` for automatic expert-cache resizing. The wrapper supports one GPU; select its index or UUID with `--gpu`. A model config's `server.gpu` must be a single index.

`--setup MODEL --config FILE` writes wrapper settings for the selected model, checkout, and GPU UUID. Existing files are never overwritten. For a temporary session, pass policy options directly.

## Settings

All fields are optional. Settings files reject unknown fields. Profile objects accept the same fields, except nested `profiles`.

| Field | Default |
| --- | --- |
| `schema_version` | `1` |
| `basalt_dir` | `~/apps/basalt` |
| `model` | none |
| `gpu` | model's `server.gpu`, else index 0 |
| `port` | model's `server.port`, else 8080 |
| `unit` | `basalt-wrap` |
| `theme` | `basalt` |
| `age_seconds` | `false` |
| `vram.enabled` | `true` |
| `vram.growth_mode` | `idle` |
| `vram.min_free_mib` | greater of 1024 and GPU memory / 16 |
| `vram.grow_below_mib` | GPU memory minus reserve minus 1024 |
| `vram.grow_delay_seconds` | 60 |
| `vram.poll_seconds` | 2 |
| `vram.step_mib` | 512 |
| `vram.accounting_margin_mib` | 16 |
| `vram.no_op_retry_seconds` | 60 |

`gpu` accepts an integer or a UUID string. `model` is a model JSON path, `iq3_s`, or `ud-iq3_s`. `profiles` maps names to settings objects. Relative paths resolve from the wrapper settings file; `~` expands to the home directory.

## Memory policy

The reserve is the minimum desired free VRAM. Below it, the wrapper requests a shrink. Growth needs enough free VRAM for one step plus reserve and accounting margin, usage below the growth threshold, and room below the engine's full expert-cache size. A growth step may carry usage above the threshold.

`idle` mode requires a continuous period without active or queued inference. `capacity` requires sustained spare VRAM and can queue a resize during inference. The engine applies it between requests. Stale monitoring data or unavailable GPU counters interrupt the timer.

Only one resize can be pending. Known busy responses defer it. Unknown failures pause management; press `r` after resolving the issue. A resize that frees no useful space gets a cooldown to avoid repeated requests. Space pauses growth while pressure shrinking stays enabled.

Leave enough free RAM for an elastic cache shrink: Basalt's resident expert mode copies released GPU experts into RAM and can refuse a shrink when its RAM floor would be crossed.

## Live controls

`--attach` connects to the worker's control socket, or directly to a server when given its model JSON. Quitting an attachment leaves the server running. `--diagnose` reports policy state as JSON without resizing. A direct server attachment cannot change the worker's policy.

Press `c` to preview settings from disk and Enter to apply the VRAM policy. Other settings take effect on the next launch. `--no-config` disables reload. Theme selections save to the selected wrapper settings/profile; command-line palette overrides do not save automatically.

Control sockets live under `$XDG_RUNTIME_DIR/basalt-wrap`. The systemd unit owns the entire process tree so a stopped or failed launcher cannot leave an engine behind. Separate units still cannot own the same port.

`BASALT_WRAP_API_KEY` sets the server key and authenticates monitoring. Basalt does not accept keys in JSON. The wrapper forwards `BASALT_*` environment variables to its worker. Engine settings remain in the model config or follow `--` as Basalt server options.
