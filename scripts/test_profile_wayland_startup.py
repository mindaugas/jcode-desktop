import importlib.util
import os
from pathlib import Path
import socket
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parent))
spec = importlib.util.spec_from_file_location('wayland_startup', Path(__file__).with_name('profile-wayland-startup.py'))
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)


class WaylandStartupTests(unittest.TestCase):
    def test_only_own_visible_native_toplevel(self):
        node = dict(pid=123, app_id='jcode-desktop', visible=True, rect={'width': 100})
        self.assertIs(profile.desktop_node({'floating_nodes': [node]}, 123), node)
        self.assertIsNone(profile.desktop_node(node, 124))
        self.assertIsNone(profile.desktop_node(dict(node, visible=False), 123))
        self.assertIsNone(profile.desktop_node(dict(node, app_id=None), 123))

    def test_startup_draft_is_editable_without_connected_history(self):
        self.assertTrue(profile.editable_panel({'session': 'startup://draft', 'history_loaded': False}))
        self.assertTrue(profile.editable_panel({'session': 'session_test', 'history_loaded': True}))
        self.assertFalse(profile.editable_panel({'session': 'session_test', 'history_loaded': False}))
        self.assertFalse(profile.editable_panel({'session': 'desktop://changelog', 'history_loaded': True}))
        self.assertFalse(profile.editable_panel(None))

    def test_pixels_restricted_to_client(self):
        before = Image.new('RGB', (20, 20))
        after = before.copy()
        rect = dict(x=5, y=5, width=5, height=5)
        after.putpixel((0, 0), (255, 0, 0))
        self.assertFalse(profile.changed_client_pixels(before, after, rect))
        after.putpixel((6, 6), (255, 0, 0))
        self.assertTrue(profile.changed_client_pixels(before, after, rect))

    def test_partial_socket_reads_and_eof(self):
        left, right = socket.socketpair()
        try:
            right.sendall(b'abc')
            self.assertEqual(profile.recv_exact(left, 3), b'abc')
            right.close()
            with self.assertRaisesRegex(RuntimeError, 'disconnected'):
                profile.recv_exact(left, 1)
        finally:
            left.close()
            right.close()

    def test_isolated_environment_and_explicit_flags(self):
        args = SimpleNamespace(compositor_renderer='pixman', render_node=None, timeout=30, sway_prefix=Path('/private/sway'), software_presentation=True,
                               **{flag.lower(): '0' if index % 2 else '1'
                                  for index, flag in enumerate(profile.FLAGS)})
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {
                'WAYLAND_DISPLAY': 'live-wayland', 'SWAYSOCK': '/live/sway', 'DISPLAY': ':0',
                'GPUI_BACKGROUND_FONTS': 'inherited', 'OPENAI_API_KEY': 'secret'}):
            env = profile.sample_env(Path(directory), Path('/fake/jcode'), Path('/intel_icd.json'), args)
            for forbidden in ('WAYLAND_DISPLAY', 'SWAYSOCK', 'DISPLAY', 'OPENAI_API_KEY', 'JCODE_DESKTOP_SCREENSHOT'):
                self.assertNotIn(forbidden, env)
            for flag in profile.FLAGS:
                self.assertEqual(env[flag], getattr(args, flag.lower()))
            self.assertEqual(env['WLR_RENDERER'], 'pixman')
            self.assertEqual(env['WLR_BACKENDS'], 'headless')
            self.assertEqual(env['MESA_VK_WSI_DEBUG'], 'sw')
            self.assertNotIn('LIBGL_ALWAYS_SOFTWARE', env)
            self.assertTrue(env['JCODE_SOCKET'].startswith(directory))

    def test_cleanup_and_failure_evidence(self):
        args = SimpleNamespace(compositor_renderer='pixman', render_node=None, timeout=1,
                               sway_prefix=None, software_presentation=False,
                               **{flag.lower(): None for flag in profile.FLAGS})
        with tempfile.TemporaryDirectory() as directory, patch.object(profile.shared, 'Children') as children:
            children.return_value.launch.side_effect = RuntimeError('compositor failed')
            root = Path(directory) / 'sample'
            with self.assertRaisesRegex(RuntimeError, 'compositor failed'):
                profile.run_sample(root, 0, args, Path('/fake/desktop'),
                                   Path('/fake/jcode'), Path('/lvp_icd.json'))
            children.return_value.close.assert_called_once()
            self.assertIn('compositor failed', (root / 'sample.json').read_text())

    def test_gpu_compositor_environment(self):
        args = SimpleNamespace(compositor_renderer='gles2', render_node=Path('/dev/dri/renderD128'),
                               timeout=30, sway_prefix=None, software_presentation=False,
                               **{flag.lower(): None for flag in profile.FLAGS})
        with tempfile.TemporaryDirectory() as directory:
            env = profile.sample_env(Path(directory), Path('/fake/jcode'), Path('/intel_icd.json'), args)
            self.assertEqual(env['WLR_RENDERER'], 'gles2')
            self.assertEqual(env['WLR_RENDER_DRM_DEVICE'], '/dev/dri/renderD128')
            self.assertNotIn('LIBGL_ALWAYS_SOFTWARE', env)
            self.assertNotIn('MESA_VK_WSI_DEBUG', env)

    def test_unspecified_flags_are_not_inherited(self):
        args = SimpleNamespace(compositor_renderer='pixman', render_node=None, timeout=30, sway_prefix=None, software_presentation=False,
                               **{flag.lower(): None for flag in profile.FLAGS})
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {flag: '1' for flag in profile.FLAGS}):
            env = profile.sample_env(Path(directory), Path('/fake/jcode'), Path('/lvp_icd.json'), args)
            for flag in profile.FLAGS:
                self.assertNotIn(flag, env)
            self.assertNotIn('MESA_VK_WSI_DEBUG', env)


if __name__ == '__main__':
    unittest.main()
