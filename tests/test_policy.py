import unittest
from dataclasses import replace

from strata_wrap.vram import Policy, Sample


def sample(**values):
    return replace(Sample(16303, 13000, 3200, 6000, 8000, (1, 0, 0)), **values)


class PolicyTests(unittest.TestCase):
    def test_shrink_repeats_only_after_confirmed_completion_and_poll(self):
        policy = Policy()
        low = sample(used=15500, free=700)
        action = policy.observe(0, low)
        self.assertEqual((action.direction, action.reserve), ("shrink", 1212))
        self.assertIsNone(policy.observe(10, low))
        self.assertTrue(policy.complete(10, {"expert_cache_mib": 5488}))
        lower = replace(low, cache=5488)
        self.assertIsNone(policy.observe(11, lower))
        self.assertEqual(policy.observe(12, lower).direction, "shrink")

    def test_growth_needs_continuous_idle_then_new_wait_after_each_step(self):
        policy = Policy()
        idle = sample()
        policy.observe(0, idle)
        policy.observe(2, idle)
        for t in range(4, 62, 2):
            self.assertIsNone(policy.observe(t, idle))
        action = policy.observe(62, idle)
        self.assertEqual((action.direction, action.reserve), ("grow", 2672))
        self.assertTrue(policy.complete(63, {"expert_cache_mib": 6512}))
        grown = replace(idle, cache=6512, used=13512, free=2688)
        policy.observe(64, grown)
        for t in range(66, 124, 2):
            self.assertIsNone(policy.observe(t, grown))
        self.assertEqual(policy.observe(124, grown).direction, "grow")

    def test_short_inference_between_polls_resets_growth(self):
        policy = Policy(delay=6)
        idle = sample()
        policy.observe(0, idle)
        policy.observe(2, idle)
        policy.observe(4, idle)
        after = replace(idle, activity=(1, 1, 5))
        self.assertIsNone(policy.observe(6, after))
        for t in (8, 10, 12):
            self.assertIsNone(policy.observe(t, after))
        self.assertEqual(policy.observe(14, after).direction, "grow")

    def test_busy_and_pressure_and_missing_data_reset_growth(self):
        for interruption in (sample(busy=True), sample(used=14500)):
            with self.subTest(interruption=interruption):
                policy = Policy(delay=6)
                policy.observe(0, sample())
                policy.observe(2, sample())
                policy.observe(4, interruption)
                for t in (6, 8, 10):
                    self.assertIsNone(policy.observe(t, sample()))
                self.assertEqual(policy.observe(12, sample()).direction, "grow")
        policy = Policy(delay=6)
        policy.observe(0, sample())
        policy.observe(2, sample())
        self.assertIsNone(policy.observe(20, sample()))
        policy.interrupt()
        self.assertIsNone(policy.observe(22, sample()))

    def test_noop_stops_retries_until_meaningful_change(self):
        policy = Policy()
        low = sample(free=700)
        policy.observe(0, low)
        self.assertFalse(policy.complete(1, {"expert_cache_mib": low.cache}))
        for t in range(4, 50, 2):
            self.assertIsNone(policy.observe(t, replace(low, free=690)))
        self.assertEqual(policy.observe(50, replace(low, free=180)).direction, "shrink")

    def test_growth_never_breaks_reserve_and_full_cache_never_grows(self):
        for idle in (sample(cache=8000), sample(free=1500)):
            policy = Policy(delay=0)
            self.assertIsNone(policy.observe(0, idle))
            self.assertIsNone(policy.observe(2, idle))

    def test_failed_resize_does_not_spin(self):
        policy = Policy()
        low = sample(free=700)
        policy.observe(0, low)
        self.assertFalse(policy.complete(1, error=True))
        self.assertIsNone(policy.observe(4, low))

    def test_resize_from_another_client_resets_growth_timer(self):
        policy = Policy(delay=6)
        idle = sample()
        policy.observe(0, idle)
        policy.observe(2, idle)
        resized = replace(idle, cache=5488)
        self.assertIsNone(policy.observe(4, resized))
        for t in (6, 8, 10):
            self.assertIsNone(policy.observe(t, resized))
        self.assertEqual(policy.observe(12, resized).direction, "grow")


if __name__ == "__main__":
    unittest.main()
