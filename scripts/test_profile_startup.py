import contextlib
import importlib.util
import io
import json
import os
import sys
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch


spec = importlib.util.spec_from_file_location('profile_startup', Path(__file__).with_name('profile-startup.py'))
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)


class StartupProfileTests(unittest.TestCase):
    def test_navigation_ignores_missing_partial_and_non_object_writes(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'state'
            self.assertEqual(profile.navigation(path), {})
            for content in ('navigation={', 'layout=Full', 'navigation=[]', 'navigation=null'):
                path.write_text(content)
                self.assertEqual(profile.navigation(path), {})
            state = {'keyboard_panel': 3, 'rows': [{'panels': [
                {'slot': 3, 'focused': True, 'session': 'startup://draft'}]}]}
            path.write_text('layout=Full\nnavigation=' + json.dumps(state) + '\n')
            self.assertEqual(profile.navigation(path), state)
            self.assertEqual(profile.focused_panel(state)['session'], 'startup://draft')
            state['keyboard_panel'] = 4
            self.assertIsNone(profile.focused_panel(state))

    def test_percentiles_include_first_sample(self):
        self.assertEqual(profile.summary([900, 10, 20, 30]),
                         dict(count=4, min_ms=10, median_ms=25, p95_ms=900, max_ms=900))
        self.assertEqual(profile.summary([4])['p95_ms'], 4)

    def test_window_watcher_uses_fine_polling_and_reports_observation(self):
        replies = [Mock(returncode=1, stdout='', stderr=''),
                   Mock(returncode=0, stdout='12345\n', stderr='')]
        with patch.object(profile.subprocess, 'run', side_effect=replies) as run:
            watcher = profile.WindowWatch({'DISPLAY': ':123'}, 1)
            watcher.thread.join(timeout=1)
            watcher.close()
        self.assertIsNone(watcher.error)
        self.assertEqual(watcher.window_id, '12345')
        self.assertIsNotNone(watcher.seen_at)
        self.assertEqual(run.call_count, 2)
        self.assertNotIn('--sync', run.call_args.args[0])

    def test_window_watcher_surfaces_failure_instead_of_timing_out(self):
        with patch.object(profile.subprocess, 'run', return_value=Mock(
                returncode=2, stdout='', stderr='cannot open display')):
            watcher = profile.WindowWatch({}, 1)
            watcher.thread.join(timeout=1)
            watcher.close()
        self.assertIn('cannot open display', str(watcher.error))
        self.assertIsNone(watcher.window_id)

    def test_cleanup_signals_owned_groups_even_if_leader_exited(self):
        children = profile.Children(Path('/unused'), {})
        process = Mock(pid=123, stdout=None)
        process.poll.return_value = 0
        children.processes.append(('desktop', process))
        with patch.object(profile.os, 'killpg') as kill:
            children.close()
        self.assertEqual(kill.call_args_list[0].args, (123, profile.signal.SIGTERM))
        self.assertEqual(kill.call_args_list[1].args, (123, profile.signal.SIGKILL))
        process.wait.assert_called()

    def test_main_snapshots_binary_away_from_packaged_companion(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / 'jcode-desktop'
            binary.write_bytes(b'host binary')
            companion = root / 'jcode'
            companion.write_bytes(b'companion binary')
            driver = root / 'intel_icd.json'
            driver.write_text('{}')
            output = root / 'out'

            def sample(path, index, args, staged, selected_companion, driver):
                self.assertEqual(staged.read_bytes(), binary.read_bytes())
                self.assertEqual(staged, output / 'bin/jcode-desktop')
                self.assertFalse(staged.with_name('jcode').exists())
                self.assertEqual(selected_companion, companion)
                self.assertTrue(args.single_panel)
                self.assertTrue(args.trace_startup)
                self.assertTrue(args.require_hardware)
                self.assertTrue(args.software_presentation)
                self.assertEqual(args.pipeline_workers, 4)
                self.assertEqual(driver, root / 'intel_icd.json')
                return dict(sample=index, first_window_ms=10, render_state_ms=20, focused_panel_ms=20)

            argv = ['profile-startup.py', str(output), '--binary', str(binary),
                    '--jcode', str(companion), '--samples', '2', '--no-verify-input',
                    '--single-panel', '--trace-startup', '--driver', str(driver),
                    '--require-hardware', '--software-presentation', '--pipeline-workers', '4']
            with patch.object(sys, 'argv', argv), \
                    patch.object(profile.shutil, 'which', return_value='/bin/true'), \
                    patch.object(Path, 'glob', return_value=iter([Path('/driver.json')])), \
                    patch.object(profile, 'run_sample', side_effect=sample), \
                    contextlib.redirect_stdout(io.StringIO()):
                profile.main()
            result = json.loads((output / 'results.json').read_text())
            self.assertFalse(result['hot_reload'])
            self.assertTrue(result['single_panel'])
            self.assertTrue(result['trace_startup'])
            self.assertTrue(result['require_hardware'])
            self.assertTrue(result['software_presentation'])
            self.assertEqual(result['pipeline_workers'], 4)
            self.assertEqual(result['driver'], str(driver))
            self.assertEqual(result['binary_sha256'], profile.sha256(binary))
            self.assertEqual(result['summary']['render_state_ms']['count'], 2)

    def test_driver_and_trace_environment_do_not_force_software_on_hardware(self):
        for driver, software in [('intel_icd.json', False), ('lvp_icd.json', True)]:
            with self.subTest(driver=driver), tempfile.TemporaryDirectory() as temporary:
                env = profile.sample_env(Path(temporary), Path('/jcode'), Path('/' + driver),
                                         60, trace_startup=True, software_presentation=True,
                                         pipeline_workers=4)
                self.assertEqual(env['MESA_VK_WSI_DEBUG'], 'sw')
                self.assertEqual(env['VK_DRIVER_FILES'], '/' + driver)
                self.assertEqual('LIBGL_ALWAYS_SOFTWARE' in env, software)
                self.assertEqual(env['JCODE_DESKTOP_TRACE_STARTUP'], '1')
                self.assertEqual(env['GPUI_PIPELINE_TIMING'], '1')
                self.assertEqual(env['GPUI_PIPELINE_WORKERS'], '4')

    def test_startup_experiments_are_explicit_and_isolated(self):
        for enabled in (False, True):
            with self.subTest(enabled=enabled), tempfile.TemporaryDirectory() as temporary:
                env = profile.sample_env(Path(temporary), Path('/jcode'), Path('/intel_icd.json'),
                                         60, early_present=enabled, background_fonts=enabled,
                                         vulkan_first=enabled, early_window=enabled)
                for key in ('GPUI_EARLY_PRESENT', 'GPUI_BACKGROUND_FONTS',
                            'GPUI_VULKAN_FIRST', 'JCODE_DESKTOP_EARLY_WINDOW'):
                    self.assertEqual(env[key], '1' if enabled else '0')

    def test_environment_is_private_and_has_no_fixture_or_real_secrets(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with patch.dict(os.environ, {'ANTHROPIC_API_KEY': 'secret', 'OPENAI_API_KEY': 'secret',
                                         'DISPLAY': ':0', 'SSH_AUTH_SOCK': '/real/agent',
                                         'JCODE_API_SOCKET': '/real/api.sock',
                                         'GPUI_EARLY_PRESENT': '1', 'GPUI_BACKGROUND_FONTS': '1',
                                         'GPUI_VULKAN_FIRST': '1', 'JCODE_DESKTOP_EARLY_WINDOW': '1'}):
                env = profile.sample_env(root, Path('/opt/private jcode'), Path('/driver.json'), 60)
            for key in ('ANTHROPIC_API_KEY', 'OPENAI_API_KEY', 'DISPLAY', 'SSH_AUTH_SOCK',
                        'JCODE_DESKTOP_SCREENSHOT', 'JCODE_DESKTOP_UI', 'MESA_VK_WSI_DEBUG',
                        'GPUI_PIPELINE_WORKERS', 'GPUI_PIPELINE_TIMING', 'GPUI_EARLY_PRESENT',
                        'GPUI_BACKGROUND_FONTS', 'GPUI_VULKAN_FIRST', 'JCODE_DESKTOP_EARLY_WINDOW'):
                self.assertNotIn(key, env)
            for key in ('HOME', 'XDG_RUNTIME_DIR', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME',
                        'XDG_DATA_HOME', 'XDG_STATE_HOME', 'JCODE_HOME', 'JCODE_API_SOCKET',
                        'JCODE_SOCKET', 'JCODE_DESKTOP_CONFIG', 'TMPDIR'):
                self.assertTrue(Path(env[key]).is_relative_to(root), (key, env[key]))
            self.assertEqual(env['PATH'], str(root / 'shims') + ':/usr/bin:/bin')
            self.assertEqual((root / 'home').stat().st_mode & 0o777, 0o700)
            self.assertIn("exec '/opt/private jcode' --no-update --no-selfdev --provider jcode serve",
                          (root / 'shims/jcode').read_text())


if __name__ == '__main__':
    unittest.main()
