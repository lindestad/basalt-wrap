"""VRAM decisions, independent of HTTP and the clock."""

from dataclasses import dataclass


@dataclass(frozen=True)
class Sample:
    total: int
    used: int
    free: int
    cache: int
    full: int
    activity: tuple
    busy: bool = False


@dataclass(frozen=True)
class Resize:
    direction: str
    reserve: int
    before: int


class Policy:
    def __init__(self, min_free=1024, grow_below=14336, step=512, delay=60, poll=2):
        self.min_free = min_free
        self.grow_below = grow_below
        self.step = step
        self.delay = delay
        self.poll = poll
        self.quiet_since = None
        self.activity = None
        self.next_shrink = 0.0
        self.pending = None
        self.blocked = {}
        self.seen_at = None
        self.last_cache = None

    def interrupt(self):
        # Missing observations cannot count as sustained spare capacity.
        self.quiet_since = None
        self.seen_at = None

    def observe(self, now, sample):
        if self.seen_at is not None and now - self.seen_at > max(5, self.poll * 3):
            self.quiet_since = None
        self.seen_at = now
        changed = sample.activity != self.activity
        self.activity = sample.activity
        resized = self.last_cache is not None and sample.cache != self.last_cache
        self.last_cache = sample.cache
        if sample.busy or changed or resized or sample.used >= self.grow_below:
            self.quiet_since = None
        elif self.quiet_since is None:
            self.quiet_since = now

        if self.pending:
            return None
        # A no-op is retried only after meaningful conditions change. Small GPU
        # accounting fluctuations must not trigger the same call every poll.
        for direction, old in list(self.blocked.items()):
            if (
                sample.cache != old.cache
                or sample.full != old.full
                or sample.activity != old.activity
                or abs(sample.free - old.free) >= self.step
            ):
                del self.blocked[direction]

        action = None
        if sample.free < self.min_free and now >= self.next_shrink:
            if "shrink" not in self.blocked:
                action = Resize("shrink", sample.free + self.step, sample.cache)
        elif (
            not sample.busy
            and sample.used < self.grow_below
            and sample.cache < sample.full
            and "grow" not in self.blocked
            and self.quiet_since is not None
            and now - self.quiet_since >= self.delay
            and sample.free - self.step - 16 >= self.min_free
        ):
            # NVML/nvidia-smi can report a little more free memory than CUDA.
            # Without a margin, CUDA may see only 511 MiB and map no segment.
            action = Resize("grow", sample.free - self.step - 16, sample.cache)
        if action:
            self.pending = (action, sample)
            self.quiet_since = None
        return action

    def complete(self, now, reply=None, error=False):
        action, sample = self.pending
        self.pending = None
        self.quiet_since = None
        self.next_shrink = now + self.poll
        if error:
            self.blocked[action.direction] = sample
            return False
        cache = reply.get("expert_cache_mib")
        if isinstance(cache, (int, float)):
            self.last_cache = cache
        changed = isinstance(cache, (int, float)) and (
            cache < action.before
            if action.direction == "shrink"
            else cache > action.before
        )
        if not changed:
            self.blocked[action.direction] = sample
        return changed
