"""Compact terminal output; raw server output is retained separately."""

import os
import sys
import threading


class Console:
    def __init__(self, stream=None):
        self.stream = stream or sys.stdout
        self.interactive = self.stream.isatty()
        self.color = self.interactive and "NO_COLOR" not in os.environ
        self.lock = threading.RLock()
        self.progress = False
        self.loaded = None
        self.watermark = 0.0
        self.finished = 0
        self.tokens = 0
        self.frame = 0

    def clear(self):
        with self.lock:
            if self.progress:
                self.stream.write("\r\033[K")
                self.stream.flush()
                self.progress = False

    def say(self, text, marker="::", color=36):
        with self.lock:
            self.clear()
            prefix = f"\033[1;{color}m{marker}\033[0m" if self.color else marker
            print(f"{prefix} {text}", file=self.stream, flush=True)

    def update(self, data):
        engine, live = data.get("engine", {}), data.get("live", {})
        model = engine.get("model")
        state = live.get("state")
        loaded = state != "unloaded"
        if loaded != self.loaded:
            self.say(
                f"{model or 'model'} {'loaded' if loaded else 'unloaded'}",
                "◆" if loaded else "◇",
                32 if loaded else 33,
            )
            self.loaded = loaded
        if self.interactive and state in ("reading", "generating"):
            elapsed = float(live.get("elapsed_s") or 0)
            if state == "reading":
                read, total = live.get("prompt_read"), live.get("prompt_total")
                if read is not None and total:
                    ratio = min(1, read / total)
                    bar = "■" * int(ratio * 30) + "·" * (30 - int(ratio * 30))
                    text = f"{bar}  prompt {read:,}/{total:,} tokens | {elapsed:.1f}s"
                else:
                    text = f"reading prompt | {elapsed:.1f}s"
            else:
                spinner = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"[self.frame % 10]
                text = (
                    f"{spinner} output {int(live.get('generated') or 0):,} tokens | "
                    f"{float(live.get('tok_s') or 0):.1f} tok/s | {elapsed:.1f}s"
                )
                self.frame += 1
            with self.lock:
                self.stream.write(f"\r{text}\033[K")
                self.stream.flush()
                self.progress = True
        else:
            self.clear()
        for item in sorted(
            data.get("requests", []), key=lambda item: item.get("time", 0)
        ):
            stamp = float(item.get("time") or 0)
            if stamp <= self.watermark:
                continue
            self.watermark = stamp
            self.finished += 1
            n = int(item.get("output_tokens") or 0)
            self.tokens += n
            read = item.get("prompt_read")
            if read is None:
                read = max(
                    0,
                    int(item.get("prompt_tokens") or 0) - int(item.get("reused") or 0),
                )
            ms = float(item.get("prompt_ms") or 0)
            self.say(
                f"prompt {int(read):,} processed, {int(item.get('reused') or 0):,} cached"
                + (f" | {read * 1000 / ms:.1f} tok/s" if ms else ""),
                "▸",
                35,
            )
            self.say(
                f"output {n:,} tokens | {float(item.get('decode_tok_s') or 0):.1f} tok/s"
                f" | {float(item.get('duration_s') or 0):.1f}s | {item.get('finish', 'done')}",
                "▸",
                35,
            )
            offered = item.get("drafts_offered") or 0
            if offered:
                accepted = item.get("drafts_accepted") or 0
                self.say(
                    f"MTP {accepted:,}/{offered:,} accepted ({100 * accepted / offered:.1f}%)",
                    "▸",
                    35,
                )

    def summary(self):
        self.say(
            f"session: {self.finished} requests, {self.tokens:,} output tokens", "▸", 35
        )
