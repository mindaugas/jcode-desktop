"""Headless tests for optional, fixture-only gperftools stack capture."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
spec = importlib.util.spec_from_file_location('perf_cpu_suite', Path(__file__).with_name('perf-suite.py'))
suite = importlib.util.module_from_spec(spec)
spec.loader.exec_module(suite)


class CpuStackTests(unittest.TestCase):
    def test_args_are_optional_and_mutually_exclusive(self):
        self.assertFalse(suite.parse_args(['out']).cpu_stacks)
        with tempfile.TemporaryDirectory() as directory:
            library = Path(directory) / 'libprofiler.so'
            library.touch()
            args = suite.parse_args(['out', '--cpu-stacks', '--cpu-profiler-lib', str(library),
                                     '--cpu-frequency', '250'])
            self.assertEqual(args.cpu_profiler_lib, library.resolve())
            self.assertEqual(args.cpu_frequency, 250)
        for extra in (['--cpu-stacks', '--profile'], ['--cpu-frequency', '0'],
                      ['--cpu-frequency', '1001'], ['--cpu-frequency', '250'],
                      ['--cpu-profiler-lib', '/missing'],
                      ['--cpu-stacks', '--cpu-profiler-lib', '/missing']):
            with self.subTest(extra=extra), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit):
                    suite.parse_args(['out', *extra])

    def test_only_fixture_gets_preload_and_signal_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            app = suite.App.__new__(suite.App)
            app.root = Path(directory)
            app.env = suite.isolated_env(app.root)
            original = app.env.copy()
            app.cpu_stacks = suite.CpuStacks(app.root, Path('/usr/lib/libprofiler.so'), 100)
            app.logs, app.processes = [], []
            try:
                with patch.object(suite.subprocess, 'Popen') as popen:
                    for name in ('sway', 'xvfb', 'wm', 'app'):
                        app._launch(name, [name])
                        env = popen.call_args.kwargs['env']
                        if name == 'app':
                            self.assertEqual(env['LD_PRELOAD'], '/usr/lib/libprofiler.so')
                            self.assertEqual(env['CPUPROFILESIGNAL'], str(int(signal.SIGUSR2)))
                            self.assertEqual(env['CPUPROFILE_FREQUENCY'], '100')
                        else:
                            self.assertFalse(any(k.startswith('CPUPROFILE') or k == 'LD_PRELOAD' for k in env))
                self.assertEqual(app.env, original)
            finally:
                for log in app.logs:
                    log.close()

    def test_signal_toggle_is_idempotent_on_stop_and_counts_windows(self):
        profiler = suite.CpuStacks(Path('/private'), Path('/library'), 100)
        process = Mock()
        process.poll.return_value = None
        with patch.object(profiler, '_wait') as wait:
            profiler.stop()
            profiler.start(process)
            self.assertEqual(profiler.path, Path('/private/cpu.prof.0'))
            profiler.stop()
            profiler.stop()
            profiler.start(process)
            self.assertEqual(profiler.path, Path('/private/cpu.prof.1'))
            profiler.stop()
        self.assertEqual(process.send_signal.call_count, 4)
        self.assertEqual([c.kwargs['stopped'] for c in wait.call_args_list], [False, True, False, True])

    def test_flush_timeout_never_restarts_profiler(self):
        profiler = suite.CpuStacks(Path('/private'), Path('/library'), 100)
        process = Mock()
        process.poll.return_value = None
        with patch.object(profiler, '_wait') as wait:
            profiler.start(process)
            wait.side_effect = RuntimeError('flush timeout')
            for _ in range(2):
                with self.assertRaisesRegex(RuntimeError, 'flush timeout'):
                    profiler.stop()
        self.assertEqual(process.send_signal.call_count, 2)

    def test_close_flushes_before_shutdown_even_if_flush_fails(self):
        app = suite.App.__new__(suite.App)
        app.cpu_stacks, process = Mock(), Mock()
        app.processes, app.logs, app.runtime_alias = [process], [], None
        events = []
        def flush():
            events.append('flush')
            raise RuntimeError('failed flush')
        app.cpu_stacks.stop.side_effect = flush
        with patch.object(app, '_stop', side_effect=lambda p: events.append('shutdown')):
            with self.assertRaisesRegex(RuntimeError, 'failed flush'):
                app.close()
        self.assertEqual(events, ['flush', 'shutdown'])

    def test_measure_failure_flushes_before_keyboard_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            app = suite.App.__new__(suite.App)
            app.root, app.env, app.display = Path(directory), {}, 'xvfb'
            (app.root / 'runtime').mkdir()
            app.app = Mock(pid=123)
            app.app.poll.side_effect = RuntimeError('input failed')
            app.cpu_stacks = Mock()
            events = []
            app.cpu_stacks.start.side_effect = lambda p: events.append('start')
            app.cpu_stacks.stop.side_effect = lambda: events.append('flush')
            with patch.object(suite.time, 'sleep'), patch.object(suite, 'process_sample', return_value={}), \
                    patch.object(app, '_stop', side_effect=lambda *a, **k: events.append('cleanup')):
                with self.assertRaisesRegex(RuntimeError, 'input failed'):
                    app.measure('idle', [], 1)
            self.assertEqual(events, ['start', 'flush', 'cleanup', 'cleanup'])
            self.assertFalse((app.root / 'runtime/jcode-desktop-profile.json').exists())

    def test_sampled_runs_record_overhead_and_skip_baseline_comparison(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary, library = root / 'binary', root / 'libprofiler.so'
            binary.write_bytes(b'fixture')
            library.touch()
            baseline = root / 'baseline.json'
            baseline.write_text(json.dumps({'idle/retention-on': {'draw_mean_ms': 1}}))
            args = suite.parse_args([str(root / 'capture'), '--binary', str(binary),
                                     '--cpu-stacks', '--cpu-profiler-lib', str(library),
                                     '--scenario', 'idle', '--repeats', '1', '--retention', 'on',
                                     '--baseline', str(baseline)])
            app = Mock()
            app.measure.return_value = dict(animation_fps=None, draw_mean_ms=99, draw_p95_max_ms=99,
                                            main_thread_cpu_percent=99, cpu_percent=99, actions=0, seconds=3)
            with patch.object(suite, 'parse_args', return_value=args), patch.object(suite, 'App', return_value=app), \
                    patch.object(suite.time, 'sleep'), patch.object(suite, 'compare') as compare, \
                    contextlib.redirect_stdout(io.StringIO()) as output:
                self.assertEqual(suite.main(), 0)
            compare.assert_not_called()
            self.assertIn('Baseline comparison skipped', output.getvalue())
            meta = json.loads((args.output / 'capture.json').read_text())
            self.assertFalse(meta['timing_comparable'])
            self.assertEqual(meta['sampling_overhead'], 'included, not calibrated')
            self.assertEqual(meta['cpu_stack_profiler'], 'gperftools')
            summary = json.loads((args.output / 'summary.json').read_text())
            self.assertFalse(summary['idle/retention-on']['timing_comparable'])
            app.close.assert_called_once()

    def test_sigterm_unwinds_through_cleanup(self):
        with self.assertRaises(KeyboardInterrupt):
            suite.interrupted(signal.SIGTERM, None)

    @unittest.skipUnless(Path('/usr/lib/libprofiler.so').exists(), 'local gperftools not installed')
    def test_real_private_child_starts_off_and_flushes_two_windows(self):
        # No display, no live host, no ptrace. Only this child receives LD_PRELOAD.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            profiler = suite.CpuStacks(root, Path('/usr/lib/libprofiler.so'), 100)
            ready = root / 'ready'
            script = f'from pathlib import Path\nPath({str(ready)!r}).touch()\nwhile True: sum(range(10000))\n'
            with (root / 'child.log').open('w') as log:
                child = subprocess.Popen([sys.executable, '-c', script],
                                         env=profiler.environment({'PATH': '/usr/bin:/bin'}),
                                         stdout=log, stderr=log)
                try:
                    deadline = time.monotonic() + 10
                    while not ready.exists():
                        self.assertIsNone(child.poll())
                        self.assertLess(time.monotonic(), deadline)
                        time.sleep(.01)
                    self.assertEqual(list(root.glob('cpu.prof*')), [])
                    for index in range(2):
                        profiler.start(child)
                        time.sleep(.1)
                        profiler.stop()
                        self.assertGreater(profiler.path.stat().st_size, 0)
                        self.assertFalse(profiler._output_open())
                        self.assertEqual(profiler.path.name, f'cpu.prof.{index}')
                finally:
                    try:
                        profiler.stop()
                    finally:
                        suite.App._stop(child)


if __name__ == '__main__':
    unittest.main()
