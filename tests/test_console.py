import io
import unittest

from strata_wrap.console import Console


class ConsoleTests(unittest.TestCase):
    def test_finished_requests_print_once_and_preserve_cache_counts(self):
        out = io.StringIO()
        console = Console(out)
        data = {
            "engine": {"model": "test"},
            "live": {"state": "idle"},
            "requests": [
                {
                    "time": 1,
                    "prompt_tokens": 20,
                    "reused": 12,
                    "prompt_ms": 100,
                    "output_tokens": 4,
                    "decode_tok_s": 12.5,
                    "duration_s": 0.4,
                    "drafts_offered": 4,
                    "drafts_accepted": 3,
                    "finish": "stop",
                }
            ],
        }
        console.update(data)
        console.update(data)
        self.assertEqual(console.finished, 1)
        self.assertEqual(console.tokens, 4)
        self.assertIn("prompt 8 processed, 12 cached", out.getvalue())
        self.assertEqual(out.getvalue().count("output 4 tokens"), 1)
        self.assertIn("MTP 3/4 accepted (75.0%)", out.getvalue())
        self.assertNotIn("\033", out.getvalue())
