import argparse
import fcntl
import json
import logging
import os
import queue
import select
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from logging.handlers import RotatingFileHandler
from pathlib import Path

from .console import Console
from .vram import Policy, Sample

ROOT = Path(__file__).resolve().parent.parent
MODELS = {"iq3_s": "strata-iq3_s.json", "ud-iq3_s": "strata-ud-iq3_s.json"}


def arguments(argv=None):
    parser = argparse.ArgumentParser(
        description="Run Strata with a compact console and elastic VRAM management."
    )
    parser.add_argument("model", help="iq3_s, ud-iq3_s, or a Strata JSON config path")
    parser.add_argument("--strata-dir", type=Path, default=Path.home() / "apps/Strata")
    parser.add_argument("--port", type=int)
    parser.add_argument("--unit", default="strata-wrap", help="systemd user unit name")
    parser.add_argument("--min-free-mib", type=int, default=1024)
    parser.add_argument("--grow-below-mib", type=int, default=14 * 1024)
    parser.add_argument("--grow-delay", type=float, default=60)
    parser.add_argument("--poll", type=float, default=2)
    parser.add_argument(
        "--raw", action="store_true", help="also print unfiltered server output"
    )
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--owner-pid", type=int, help=argparse.SUPPRESS)
    parser.add_argument("--owner-start", help=argparse.SUPPRESS)
    args, extra = parser.parse_known_args(argv)
    if extra and extra[0] == "--":
        extra = extra[1:]
    if any(
        item.split("=", 1)[0] in ("--host", "--config", "--engine", "--api-key")
        for item in extra
    ):
        parser.error(
            "set the model config and API key in JSON; the wrapper owns --host and --engine"
        )
    if (
        args.poll < 0.2
        or args.grow_delay < 0
        or args.min_free_mib < 0
        or args.grow_below_mib <= 0
    ):
        parser.error("invalid VRAM thresholds or polling interval")
    if not args.unit or any(
        c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_."
        for c in args.unit
    ):
        parser.error("invalid unit name")
    args.unit = args.unit.removesuffix(".service") + ".service"
    args.strata_dir = args.strata_dir.expanduser().resolve()
    args.config = (
        args.strata_dir / MODELS[args.model]
        if args.model in MODELS
        else Path(args.model).expanduser().resolve()
    )
    return args, extra


def pid_start(pid):
    # comm can contain spaces and parentheses; the last ')' ends that field.
    return Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]


def launch(args, argv):
    stop = threading.Event()
    stop_requests = 0

    def shutdown(signum, frame):
        nonlocal stop_requests
        stop_requests += 1
        stop.set()

    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, shutdown)
    command = [
        "systemd-run",
        "--user",
        "--quiet",
        "--collect",
        "--wait",
        "--pipe",
        "--service-type=exec",
        f"--unit={args.unit}",
        "--expand-environment=no",
        "--property=KillMode=mixed",
        "--property=TimeoutStopSec=20s",
        "--property=SendSIGKILL=yes",
        "--property=Restart=no",
        f"--working-directory={ROOT}",
    ]
    for key, value in os.environ.items():
        if key.startswith("STRATA_") or key in (
            "TERM",
            "NO_COLOR",
            "COLORTERM",
            "CUDA_VISIBLE_DEVICES",
            "XDG_STATE_HOME",
        ):
            command.append(f"--setenv={key}={value}")
    command += [
        sys.executable,
        str(ROOT / "strata-wrap"),
        *argv,
        "--worker",
        "--owner-pid",
        str(os.getpid()),
        "--owner-start",
        pid_start(os.getpid()),
    ]
    process = subprocess.Popen(
        command, stdin=subprocess.DEVNULL, start_new_session=True
    )
    try:
        while process.poll() is None:
            if stop.wait(0.1):
                # Stop only a service actually owned by this invocation. A second
                # launch rejected by systemd must never stop the first server.
                owner = subprocess.run(
                    [
                        "systemctl",
                        "--user",
                        "show",
                        args.unit,
                        "--property=ExecStart",
                        "--value",
                    ],
                    capture_output=True,
                    text=True,
                    timeout=5,
                    check=False,
                ).stdout
                if f"--owner-pid {os.getpid()} " in owner:
                    action = (
                        ["kill", "--kill-whom=all", "--signal=SIGKILL"]
                        if stop_requests > 1
                        else ["stop", "--no-block"]
                    )
                    subprocess.run(
                        ["systemctl", "--user", *action, args.unit],
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                        timeout=5,
                        check=False,
                    )
                    stop.clear()
                else:
                    # A signal can arrive before systemd has created the unit.
                    time.sleep(0.1)
        return 130 if stop_requests else process.returncode
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


class API:
    def __init__(self, port, key):
        self.url = f"http://127.0.0.1:{port}"
        self.key = key
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def call(self, path, body=None, timeout=2):
        payload = None if body is None else json.dumps(body).encode()
        request = urllib.request.Request(self.url + path, data=payload)
        if self.key:
            request.add_header("Authorization", "Bearer " + self.key)
        if payload is not None:
            request.add_header("Content-Type", "application/json")
        with self.opener.open(request, timeout=timeout) as response:
            return json.load(response)


def gpu_memory(gpu):
    result = subprocess.run(
        [
            "nvidia-smi",
            "-i",
            str(gpu),
            "--query-gpu=memory.total,memory.used,memory.free",
            "--format=csv,noheader,nounits",
        ],
        capture_output=True,
        text=True,
        check=True,
        timeout=3,
    )
    return tuple(
        int(float(value.strip())) for value in result.stdout.strip().split(",")
    )


def activity(status):
    item = status.get("activity", {})
    return (status.get("started"), item.get("requests"), item.get("last_request_at"))


def sample_from(status, metrics, memory):
    engine = metrics.get("engine", {})
    vram = status.get("vram", {})
    cache = int(vram.get("expert_cache_mib", engine.get("expert_cache_mib", 0)))
    full = int(vram.get("expert_cache_full_mib", engine.get("expert_cache_mib", 0)))
    live = metrics.get("live", {})
    busy = (
        status.get("activity", {}).get("in_flight", 0) > 0
        or live.get("state") in ("reading", "generating")
        or bool(live.get("queued"))
    )
    return Sample(*memory, cache, full, activity(status), busy)


def log_setup(model):
    directory = (
        Path(os.environ.get("XDG_STATE_HOME", Path.home() / ".local/state"))
        / "strata-wrap"
    )
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    path = directory / f"{model}.log"
    logger = logging.getLogger("strata-wrap")
    logger.setLevel(logging.INFO)
    handler = RotatingFileHandler(path, maxBytes=8 * 1024 * 1024, backupCount=3)
    handler.setFormatter(logging.Formatter("%(asctime)s %(message)s"))
    logger.addHandler(handler)
    return logger, path


def worker(args, extra):
    config = json.loads(args.config.read_text())
    if config.get("vram_elastic") is not True:
        raise ValueError(f"enable vram_elastic in {args.config}")
    segment = int(config.get("vram_segment_mib", 512))
    if segment != 512:
        raise ValueError("the wrapper requires 512 MiB cache segments")
    port = args.port or int(config.get("port", 8080))
    # Avoid sending management requests to an unrelated server while ours fails
    # to bind. The lock also excludes two wrappers using different unit names.
    lock_path = (
        Path(os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"))
        / f"strata-wrap-{port}.lock"
    )
    lock = lock_path.open("a")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        raise ValueError(f"another wrapper owns port {port}") from None
    with socket.socket() as probe:
        if probe.connect_ex(("127.0.0.1", port)) == 0:
            raise ValueError(f"port {port} already has a server; stop it first")

    owner_fd = None
    if args.owner_pid:
        if pid_start(args.owner_pid) != args.owner_start:
            raise ValueError("launcher has exited")
        owner_fd = os.pidfd_open(args.owner_pid)
    stop = threading.Event()
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, lambda signum, frame: stop.set())

    console = Console()
    logger, log_path = log_setup(args.config.stem)

    def say(message, marker="::", color=36):
        logger.info("[wrapper] %s", message)
        console.say(message, marker, color)

    command = [
        str(args.strata_dir / ".venv/bin/python"),
        "-u",
        "serve/server.py",
        "--engine",
        "strata",
        "--config",
        str(args.config),
        *extra,
        "--host",
        "127.0.0.1",
        "--port",
        str(port),
    ]
    say(f"loading {config.get('model_name', args.model)} on 127.0.0.1:{port}")
    say(f"raw output: {log_path}; stop: systemctl --user stop {args.unit}")
    say(
        f"VRAM: shrink below {args.min_free_mib} MiB free; grow below {args.grow_below_mib} MiB used"
        f" after {args.grow_delay:g}s quiet; steps 512 MiB"
    )
    proc = subprocess.Popen(
        command,
        cwd=args.strata_dir,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
        bufsize=1,
        start_new_session=True,
    )

    def read_output():
        for line in proc.stdout:
            logger.info("[server] %s", line.rstrip())
            if args.raw or any(
                word in line.lower()
                for word in ("error", "warning", "traceback", "exception", "failed")
            ):
                console.say(line.rstrip(), "!", 33)
            elif line.startswith(
                (
                    "loading the vision",
                    "loading the model",
                    "[strata] starting the engine",
                    "[strata] loading the experts",
                    "[strata] stopping",
                )
            ):
                console.say(line.rstrip())

    reader = threading.Thread(target=read_output, daemon=True)
    reader.start()
    api = API(port, config.get("api_key") or os.environ.get("STRATA_API_KEY", ""))
    policy = Policy(
        args.min_free_mib, args.grow_below_mib, 512, args.grow_delay, args.poll
    )
    results = queue.Queue()
    previous_metrics = None
    next_api = next_gpu = 0.0
    failures = 0
    elastic_warned = False
    monitor_disabled = False
    last_error_at = -30.0

    def resize(action):
        try:
            reply = api.call("/v1/vram", {"reserve_mib": action.reserve}, timeout=430)
            if (
                not isinstance(reply, dict)
                or reply.get("status") != "ok"
                or not isinstance(reply.get("expert_cache_mib"), int)
            ):
                raise ValueError(
                    "resize response did not report a completed cache resize"
                )
            results.put((reply, None))
        except (OSError, ValueError, urllib.error.URLError) as error:
            # A timed-out request may still be waiting inside Strata. Never
            # enqueue another resize with an unknown outcome.
            results.put((None, error))

    try:
        while proc.poll() is None and not stop.is_set():
            if owner_fd is not None and select.select([owner_fd], [], [], 0)[0]:
                say("launcher exited; stopping Strata")
                break
            now = time.monotonic()
            try:
                reply, error = results.get_nowait()
            except queue.Empty:
                pass
            else:
                action = policy.pending[0]
                changed = policy.complete(now, reply, error=error is not None)
                if error:
                    say(f"VRAM resize failed: {error}", "!", 33)
                    if (
                        not isinstance(error, urllib.error.HTTPError)
                        or error.code >= 500
                    ):
                        monitor_disabled = True
                        say(
                            "VRAM management paused: resize outcome is unknown; restart the wrapper to resume",
                            "!",
                            33,
                        )
                    elif error.code == 400:
                        monitor_disabled = True
                        say(
                            "VRAM management paused: engine refused resizing; inspect the engine log",
                            "!",
                            33,
                        )
                else:
                    say(
                        f"VRAM {action.direction}: cache {action.before} -> {reply.get('expert_cache_mib')} MiB; "
                        f"{reply.get('vram_free_mib')} MiB free"
                        + (
                            " (no change; waiting for conditions to change)"
                            if not changed
                            else ""
                        )
                    )
                next_gpu = now + args.poll

            if now >= next_api:
                try:
                    status = api.call("/v1/status")
                    metrics = api.call("/metrics")
                    console.update(metrics)
                    previous_metrics = metrics
                    failures = 0
                    if not status.get("loaded"):
                        policy.interrupt()
                    elif not status.get("vram", {}).get("elastic"):
                        policy.interrupt()
                        if not elastic_warned:
                            say(
                                "engine did not enable elastic VRAM; automatic resizing is paused",
                                "!",
                                33,
                            )
                            elastic_warned = True
                    elif now >= next_gpu and not monitor_disabled:
                        last_sample = sample_from(
                            status, metrics, gpu_memory(config.get("gpu", 0))
                        )
                        action = policy.observe(time.monotonic(), last_sample)
                        if action:
                            say(
                                f"VRAM {action.direction} requested: target {action.reserve} MiB free"
                            )
                            threading.Thread(
                                target=resize, args=(action,), daemon=True
                            ).start()
                        next_gpu = time.monotonic() + args.poll
                    elif (
                        status.get("activity", {}).get("in_flight", 0)
                        or activity(status) != policy.activity
                    ):
                        policy.interrupt()
                except (
                    OSError,
                    ValueError,
                    KeyError,
                    TypeError,
                    urllib.error.URLError,
                    subprocess.SubprocessError,
                ) as error:
                    policy.interrupt()
                    failures += 1
                    if (
                        failures >= 3
                        and previous_metrics is not None
                        and now - last_error_at >= 30
                    ):
                        say(f"waiting for monitoring data: {error}", "!", 33)
                        last_error_at = now
                busy = (previous_metrics or {}).get("live", {}).get("state") in (
                    "reading",
                    "generating",
                )
                next_api = time.monotonic() + (0.4 if busy else 1)
            stop.wait(0.1)
    finally:
        console.clear()
        say("stopping Strata")
        if proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=12)
            except subprocess.TimeoutExpired:
                say(
                    "graceful shutdown timed out; killing the server; systemd will clean up its children",
                    "!",
                    33,
                )
                proc.kill()
                proc.wait(timeout=2)
        reader.join(timeout=1)
        if owner_fd is not None:
            os.close(owner_fd)
        console.summary()
        say("stopped")
    if stop.is_set():
        return 0
    return proc.returncode if proc.returncode >= 0 else 128 - proc.returncode


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    args, extra = arguments(argv)
    try:
        if args.worker:
            return worker(args, extra)
        if not args.config.is_file():
            raise ValueError(f"config not found: {args.config}")
        return launch(args, argv)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"strata-wrap: {error}", file=sys.stderr)
        return 1
