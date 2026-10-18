# strata-wrap

Run one Strata model with a compact terminal console and automatic expert-cache resizing. Python 3.10+, Linux with a systemd user manager, and one NVIDIA GPU are required. There are no Python dependencies.

```sh
python3 install.py
strata-wrap iq3_s
strata-wrap ud-iq3_s
strata-wrap /path/to/model.json
```

The installer enables `vram_elastic` with 512 MiB segments in the existing regular and UD configs under `~/apps/Strata`, creates `~/.local/bin/strata-wrap`, and redirects the Fish abbreviations `sa` and `saud`. It backs up the changed files under `~/.local/state/strata-wrap/backups`. Existing API keys, model arguments, and pricing stay in their local JSON files.

The console shows model loading, prompt progress, output tokens and speed, per-request timings, cache reuse, and MTP acceptance. When output is redirected, it prints completed request summaries without terminal escape sequences. `--raw` also displays unfiltered server output. Raw server output and resize events are always saved under `~/.local/state/strata-wrap/`, rotating at 8 MiB with three backups per model. Strata also keeps its native engine log at the path in its model config.

## VRAM policy

- Poll GPU memory every two seconds. Below 1,024 MiB free, request a 512 MiB cache reduction. Once the response confirms a reduction, wait two seconds before another shrink.
- Grow by 512 MiB only after total GPU usage stays below 14,336 MiB for 60 seconds with no inference. A resize, new inference, missing monitoring data, or usage reaching that threshold resets the timer. Each growth step requires a fresh quiet period.
- Never request growth that would leave less than 1,024 MiB free. The initial full cache is the growth limit; only the expert cache can be resized.
- Allow only one resize request at a time. A successful HTTP response counts as progress only if the returned cache size changed in the requested direction. After a no-op, retry that direction only when the cache size, inference activity, or available memory changes substantially.
- An HTTP timeout or server error leaves the resize outcome uncertain, so management pauses until the wrapper restarts. Engine refusal also pauses management. Inference continues.

Strata accepts a target amount of free memory rather than an exact cache delta. The wrapper adds or subtracts 512 MiB from the sampled free memory to calculate that target. Growth includes a 16 MiB accounting margin: NVIDIA's reported free memory can exceed CUDA's by a small amount, leaving less than a whole segment available. Concurrent allocations and segment rounding can change the actual result. A shrink can wait behind inference; the free-memory target is best effort, not an allocation guarantee. The wrapper does not unload the model to reclaim its fixed allocations.

Defaults can be changed per launch:

```sh
strata-wrap iq3_s --min-free-mib 1024 --grow-below-mib 14336 \
    --grow-delay 60 --poll 2
```

Other Strata server options can follow the model name. `--port` changes both the listener and monitoring address. The wrapper fixes the host to loopback and reads the API key from the model config. It does not proxy inference traffic or alter the model's public name.

## Process lifetime

Each launch creates the transient user service `strata-wrap.service`. It starts on demand, with no automatic restart or boot activation. A second launch is refused while it is running. Use a different `--unit` and port for a separate instance.

Ctrl+C, SIGTERM, SIGHUP, or the command below initiates shutdown:

```sh
systemctl --user stop strata-wrap
```

The wrapper sends SIGTERM to the Strata server, which takes its normal shutdown path. After 12 seconds the wrapper kills an unresponsive server. systemd contains the wrapper and all descendants in one cgroup, including children that create new process groups or sessions. Its `KillMode=mixed` and 20-second stop timeout remove remaining children when the service ends. There is no restart loop that can bring the model back after stopping it.

The foreground launcher remains alive while the service runs. The worker watches a Linux pidfd for that launcher, so even SIGKILL of the foreground command triggers cleanup. Killing the service's main wrapper also triggers systemd cleanup. This containment requires launching through `strata-wrap`; `--worker` is an internal entry point.

## Checks

```sh
python3 -m unittest discover -s tests -v
```

The runtime tests use a fake server that starts a child in a separate session and ignores SIGTERM. They exercise Ctrl+C, hangup, service stop, wrapper and launcher SIGKILL, unexpected server death, and duplicate launches against the real systemd user manager. They use port 18089 and temporary units and clean up afterward.
