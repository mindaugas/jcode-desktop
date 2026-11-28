#!/usr/bin/env python3
"""Run with target/tui-startup-venv/bin/python -m unittest discover -s scripts -p test_profile_tui_startup.py."""
import importlib.util
import os
from pathlib import Path
import pty
import subprocess
import tempfile
import termios
import time
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("profile_tui_startup", Path(__file__).with_name("profile-tui-startup.py"))
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)


class StartupTests(unittest.TestCase):
    def setUp(self):
        Path("target").mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="tui-test-", dir=Path("target").resolve())
        self.root = Path(self.temp.name)
        self.addCleanup(self.temp.cleanup)

    def test_private_environment_does_not_inherit_secrets_or_live_sockets(self):
        with patch.dict(os.environ, {"OPENAI_API_KEY": "secret", "JCODE_SOCKET": "/live.sock", "SSH_AUTH_SOCK": "/agent.sock"}):
            env = bench.isolated_env(self.root / "private")
        self.assertNotIn("OPENAI_API_KEY", env)
        self.assertNotIn("SSH_AUTH_SOCK", env)
        self.assertEqual(env["JCODE_COMPANION_AUTOSTART"], "0")
        for key in ("HOME", "JCODE_HOME", "JCODE_RUNTIME_DIR", "XDG_RUNTIME_DIR"):
            self.assertTrue(Path(env[key]).is_relative_to(self.root))
            self.assertEqual(Path(env[key]).stat().st_mode & 0o777, 0o700)
        self.assertTrue(env["JCODE_SOCKET"].startswith(str(self.root)))

    def test_pty_disables_all_local_echo(self):
        master, slave = pty.openpty()
        try:
            bench.configure_pty(slave)
            self.assertFalse(termios.tcgetattr(slave)[3] & (termios.ECHO | termios.ECHONL | termios.ICANON))
        finally:
            os.close(master)
            os.close(slave)

    def test_terminal_queries_across_every_split(self):
        for query, response in bench.QUERIES:
            for split in range(1, len(query)):
                with self.subTest(query=query, split=split), patch.object(bench.os, "write") as write:
                    pending = bench.reply_queries(1, b"display text" + query[:split])
                    self.assertEqual(write.call_count, 0)
                    self.assertEqual(bench.reply_queries(1, pending + query[split:]), b"")
                    write.assert_called_once_with(1, response)

    def test_terminal_query_buffer_does_not_retain_transcript(self):
        with patch.object(bench.os, "write"):
            self.assertEqual(bench.reply_queries(1, b"ordinary text" * 1000), b"")

    def test_meaningful_text_excludes_probe_and_borders(self):
        self.assertIsNone(bench.meaningful_line(["──────", "  ", bench.PROBE]))
        self.assertEqual(bench.meaningful_line(["  Starting   server... "]), "Starting server...")

    def test_summary_retains_failed_samples(self):
        self.assertEqual(bench.summary([None]), {"count": 0, "total": 1})
        result = bench.summary([1, None, 3])
        self.assertEqual((result["count"], result["total"], result["median_ms"]), (2, 3, 2))

    def fake_binary(self, code):
        path = self.root / "fake-jcode"
        path.write_text("#!/usr/bin/python3\n" + code)
        path.chmod(0o700)
        return path

    def test_rendered_input_and_timing_includes_popen(self):
        binary = self.fake_binary(
            "import os, time\n"
            "os.write(1, b'Welcome to test terminal\\r\\n')\n"
            "value = os.read(0, 32)\n"
            "os.write(1, b'1> ' + value)\n"
            "time.sleep(10)\n")
        real_popen = subprocess.Popen
        def delayed_popen(*args, **kwargs):
            time.sleep(.06)
            return real_popen(*args, **kwargs)
        with patch.object(bench.subprocess, "Popen", side_effect=delayed_popen):
            result = bench.run_once(binary, self.root / "run", 2)
        self.assertTrue(result["success"])
        self.assertGreaterEqual(result["first_visible_ms"], 60)
        self.assertGreaterEqual(result["input_ready_ms"], result["probe_sent_ms"])
        self.assertEqual(result["probe_rendered_line"], "1> " + bench.PROBE)
        self.assertFalse(result["cleanup"]["remaining_private_pids"])

    def test_unrendered_input_is_not_ready(self):
        binary = self.fake_binary(
            "import os, time\n"
            "os.write(1, b'Welcome to test terminal\\r\\n')\n"
            "os.read(0, 32)\n"
            "time.sleep(10)\n")
        result = bench.run_once(binary, self.root / "run", .15)
        self.assertFalse(result["success"])
        self.assertIsNone(result["input_ready_ms"])
        self.assertFalse(result["cleanup"]["remaining_private_pids"])

    def test_startup_consumed_probe_is_retried(self):
        binary = self.fake_binary(
            "import os, time\n"
            "os.write(1, b'Welcome to test terminal\\r\\n')\n"
            "os.read(0, 32)\n"
            "value = os.read(0, 32)\n"
            "os.write(1, b'1> ' + value)\n"
            "time.sleep(10)\n")
        result = bench.run_once(binary, self.root / "run", 2)
        self.assertTrue(result["success"])
        self.assertGreaterEqual(result["probe_attempts"], 2)

    def test_first_run_onboarding_is_skipped_with_native_escape(self):
        binary = self.fake_binary(
            "import os, time\n"
            "os.write(1, b'Esc to skip onboarding\\r\\n')\n"
            "assert os.read(0, 1) == b'\\x1b'\n"
            "value = os.read(0, 32)\n"
            "os.write(1, b'1> ' + value)\n"
            "time.sleep(10)\n")
        result = bench.run_once(binary, self.root / "run", 2)
        self.assertTrue(result["success"])
        self.assertIsNotNone(result["onboarding_escape_ms"])
        self.assertGreaterEqual(result["probe_sent_ms"] - result["onboarding_escape_ms"], 25)

    def test_detached_private_daemon_cleaned_without_touching_other_child(self):
        env = bench.isolated_env(self.root / "private")
        private = subprocess.Popen(["/usr/bin/sleep", "10"], env=env, start_new_session=True)
        unrelated = subprocess.Popen(["/usr/bin/sleep", "10"])
        try:
            self.assertIn(private.pid, bench.private_pids(env))
            self.assertNotIn(unrelated.pid, bench.private_pids(env))
            result = bench.cleanup(private, env)
            self.assertIn(private.pid, result["private_pids_terminated"])
            self.assertIsNone(unrelated.poll())
        finally:
            if private.poll() is None:
                private.kill()
            private.wait()
            unrelated.terminate()
            unrelated.wait()


if __name__ == "__main__":
    unittest.main()
