"""Exercise real systemd cgroup cleanup with a small fake Strata server."""

import json
import os
import signal
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


FAKE_SERVER = """
import json, os, signal, subprocess, sys, time
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

port = int(sys.argv[sys.argv.index('--port') + 1])
child = subprocess.Popen([sys.executable, '-c',
    'import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(600)'],
    start_new_session=True)
Path('pids.json').write_text(json.dumps([os.getpid(), child.pid]))
class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        data = ({'service': 'strata', 'loaded': True, 'started': 1,
                 'activity': {'requests': 0, 'in_flight': 0},
                 'vram': {'elastic': True}}
                if self.path == '/v1/status' else
                {'engine': {'model': 'fake', 'expert_cache_mib': 100},
                 'live': {'state': 'idle'}, 'requests': []})
        self.send_response(200); self.end_headers()
        self.wfile.write(json.dumps(data).encode())
print('fake started', flush=True)
ThreadingHTTPServer(('127.0.0.1', port), Handler).serve_forever()
"""


def alive(pid):
    try:
        # Zombies have exited; only their parent can reap them.
        return Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0] != "Z"
    except FileNotFoundError:
        return False


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="strata-wrap-test-")
        self.folder = Path(self.temp.name)
        (self.folder / "serve").mkdir()
        (self.folder / ".venv/bin").mkdir(parents=True)
        (self.folder / ".venv/bin/python").symlink_to(os.sys.executable)
        (self.folder / "serve/server.py").write_text(FAKE_SERVER)
        (self.folder / "strata-iq3_s.json").write_text(
            json.dumps({"vram_elastic": True, "port": 18089})
        )
        self.unit = f"strata-wrap-test-{os.getpid()}"
        self.process = None
        self.pids = []
        self.output = (self.folder / "output.log").open("w+")

    def tearDown(self):
        subprocess.run(
            ["systemctl", "--user", "stop", self.unit], capture_output=True, check=False
        )
        if self.process:
            try:
                self.process.wait(timeout=25)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        self.output.close()
        self.temp.cleanup()

    def launch(self):
        self.process = subprocess.Popen(
            [
                str(ROOT / "strata-wrap"),
                "iq3_s",
                "--strata-dir",
                str(self.folder),
                "--unit",
                self.unit,
            ],
            stdout=self.output,
            stderr=subprocess.STDOUT,
            start_new_session=True,
            env={**os.environ, "XDG_STATE_HOME": str(self.folder / "state")},
        )
        for _ in range(100):
            if (self.folder / "pids.json").exists():
                self.pids = json.loads((self.folder / "pids.json").read_text())
                return
            if self.process.poll() is not None:
                self.output.seek(0)
                self.fail(self.output.read())
            time.sleep(0.1)
        self.fail("fake server did not start")

    def assert_clean(self):
        for _ in range(250):
            if not any(alive(pid) for pid in self.pids):
                return
            time.sleep(0.1)
        self.fail(f"surviving processes: {[pid for pid in self.pids if alive(pid)]}")

    def test_ctrl_c_cleans_child_that_ignores_term_and_changes_session(self):
        self.launch()
        self.process.send_signal(signal.SIGINT)
        self.process.wait(timeout=25)
        self.assert_clean()

    def test_killed_worker_cleans_entire_cgroup(self):
        self.launch()
        subprocess.run(
            [
                "systemctl",
                "--user",
                "kill",
                "--kill-whom=main",
                "--signal=SIGKILL",
                self.unit,
            ],
            check=True,
        )
        self.process.wait(timeout=25)
        self.assert_clean()

    def test_service_stop_cleans_entire_cgroup(self):
        self.launch()
        subprocess.run(
            ["systemctl", "--user", "stop", self.unit], check=True, timeout=25
        )
        self.process.wait(timeout=5)
        self.assert_clean()

    def test_unexpected_server_death_cleans_escaped_child(self):
        self.launch()
        os.kill(self.pids[0], signal.SIGKILL)
        self.process.wait(timeout=25)
        self.assertNotEqual(self.process.returncode, 0)
        self.assert_clean()

    def test_hangup_cleans_entire_cgroup(self):
        self.launch()
        self.process.send_signal(signal.SIGHUP)
        self.process.wait(timeout=25)
        self.assert_clean()

    def test_killed_launcher_is_detected_by_worker(self):
        self.launch()
        self.process.kill()
        self.process.wait(timeout=5)
        self.assert_clean()

    def test_duplicate_launcher_cannot_stop_running_service(self):
        self.launch()
        duplicate = subprocess.run(
            [
                str(ROOT / "strata-wrap"),
                "iq3_s",
                "--strata-dir",
                str(self.folder),
                "--unit",
                self.unit,
            ],
            capture_output=True,
            timeout=5,
            check=False,
        )
        self.assertNotEqual(duplicate.returncode, 0)
        self.assertTrue(all(alive(pid) for pid in self.pids))


if __name__ == "__main__":
    unittest.main()
