import unittest

from strata_wrap.app import arguments, sample_from


class SampleTests(unittest.TestCase):
    def test_initial_and_resized_cache_sizes_follow_engine_reports(self):
        status = {
            "started": 1,
            "activity": {"requests": 3, "last_request_at": 12},
            "vram": {"elastic": True},
        }
        metrics = {"engine": {"expert_cache_mib": 5966}, "live": {"state": "idle"}}
        initial = sample_from(status, metrics, (16303, 14000, 1900))
        self.assertEqual((initial.cache, initial.full), (5966, 5966))
        status["vram"].update(expert_cache_mib=4096, expert_cache_full_mib=5966)
        resized = sample_from(status, metrics, (16303, 12000, 3900))
        self.assertEqual((resized.cache, resized.full), (4096, 5966))
        self.assertEqual(resized.activity, (1, 3, 12))

    def test_metrics_detect_busy_even_if_status_snapshot_was_idle(self):
        status = {"activity": {"in_flight": 0}}
        self.assertTrue(
            sample_from(status, {"live": {"state": "generating"}}, (16, 14, 2)).busy
        )
        self.assertTrue(
            sample_from(
                status, {"live": {"state": "idle", "queued": 1}}, (16, 14, 2)
            ).busy
        )

    def test_server_arguments_remain_available(self):
        args, extra = arguments(["iq3_s", "--idle-unload", "120", "--port", "8089"])
        self.assertEqual(args.port, 8089)
        self.assertEqual(extra, ["--idle-unload", "120"])
