#!/usr/bin/env python3
"""Measure fresh native Wayland startup on a private headless Sway compositor.

Never touches the live compositor. Sway defaults to pixman software composition;
--compositor-renderer gles2 --render-node /dev/dri/renderD128 enables isolated
GPU composition. The Desktop Vulkan ICD is selected separately. First mapped toplevel is observed by
private IPC. First captured changed client pixels is an upper bound including
screencopy/PNG overhead, NOT a wp_presentation timestamp or physical photons.
Fresh application caches, retained OS page cache. No build, reload, or prompts.
"""
import argparse
import importlib.util
import json
import math
from pathlib import Path
import shutil
import signal
import socket
import struct
import time

from PIL import Image, ImageChops

_spec = importlib.util.spec_from_file_location('startup_shared', Path(__file__).with_name('profile-startup.py'))
shared = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(shared)

FLAGS = ('GPUI_EARLY_PRESENT', 'GPUI_BACKGROUND_FONTS', 'GPUI_VULKAN_FIRST',
         'JCODE_DESKTOP_EARLY_WINDOW')


def recv_exact(stream, size):
    data = b''
    while len(data) < size:
        chunk = stream.recv(size - len(data))
        if not chunk:
            raise RuntimeError('Private Sway IPC disconnected')
        data += chunk
    return data


def ipc(path, kind=4, payload=''):
    encoded = payload.encode()
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(5)
        stream.connect(str(path))
        stream.sendall(b'i3-ipc' + struct.pack('<II', len(encoded), kind) + encoded)
        header = recv_exact(stream, 14)
        if header[:6] != b'i3-ipc':
            raise RuntimeError('Invalid Sway IPC header')
        length, _ = struct.unpack('<II', header[6:])
        if length > 16 * 1024 * 1024:
            raise RuntimeError('Oversized Sway IPC reply')
        return json.loads(recv_exact(stream, length))


def desktop_node(tree, pid):
    # PID binds evidence to our fresh process, never another application.
    if (tree.get('pid') == pid and tree.get('app_id') and tree.get('visible')
            and tree.get('rect', {}).get('width', 0) > 0):
        return tree
    for node in tree.get('nodes', []) + tree.get('floating_nodes', []):
        found = desktop_node(node, pid)
        if found:
            return found
    return None


def changed_client_pixels(before, after, rect):
    box = (rect['x'], rect['y'], rect['x'] + rect['width'], rect['y'] + rect['height'])
    previous, current = before.crop(box).convert('RGB'), after.crop(box).convert('RGB')
    return ImageChops.difference(previous, current).getbbox() is not None


def sample_env(root, jcode, driver, args):
    env = shared.sample_env(root, jcode, driver, args.timeout, trace_startup=True)
    env.update(WLR_BACKENDS='headless', WLR_RENDERER=args.compositor_renderer, WLR_LIBINPUT_NO_DEVICES='1',
               XDG_SESSION_TYPE='wayland')
    if args.render_node:
        env['WLR_RENDER_DRM_DEVICE'] = str(args.render_node)
    if args.sway_prefix:
        env['PATH'] = str(args.sway_prefix / 'usr/bin') + ':' + env['PATH']
        env['LD_LIBRARY_PATH'] = str(args.sway_prefix / 'usr/lib')
    for flag in FLAGS:
        value = getattr(args, flag.lower())
        if value is not None:
            env[flag] = value
    if args.software_presentation:
        env['MESA_VK_WSI_DEBUG'] = 'sw'
    return env


def run_sample(root, index, args, binary, jcode, driver):
    root.mkdir(mode=0o700)
    env = sample_env(root, jcode, driver, args)
    children = shared.Children(root, env)
    sample = dict(sample=index, first_mapped_ms=None, first_captured_pixels_ms=None,
                  focused_panel_ms=None, input_verified=False)

    def command(argv, **kwargs):
        return shared.subprocess.run(argv, env=env, cwd=root, check=True, timeout=15, **kwargs)

    def wait(predicate, label):
        deadline = time.perf_counter() + args.timeout
        while time.perf_counter() < deadline:
            children.check()
            value = predicate()
            if value:
                return value
            time.sleep(args.poll_ms / 1000)
        raise TimeoutError(label)

    def capture(name):
        path = root / name
        command(['grim', '-o', 'HEADLESS-1', str(path)])
        with Image.open(path) as image:
            return image.convert('RGB')

    def key(*keys):
        command(['wtype', '-s', '100', *keys])

    try:
        config = root / 'sway.config'
        config.write_text('xwayland disable\noutput HEADLESS-1 mode 1440x1000\n'
                          'seat seat0 fallback true\n'
                          'default_border none\nfocus_follows_mouse no\n')
        children.launch('sway', ['sway', '--config', str(config)])
        env['SWAYSOCK'] = str(wait(lambda: next((root / 'runtime').glob('sway-ipc.*.sock'), None), 'Sway IPC'))
        env['WAYLAND_DISPLAY'] = wait(lambda: next((p.name for p in (root / 'runtime').glob('wayland-*')
                                                   if p.is_socket()), None), 'Wayland socket')
        wait(lambda: ipc(env['SWAYSOCK'], 3), 'headless output')
        baseline = capture('baseline.png')
        # Cold private companion is spawned by Desktop through the isolated shim.
        started = time.perf_counter()
        app = children.launch('desktop', [str(binary), '--no-hot-reload'] +
                              (['--single-panel'] if args.single_panel else []))
        deadline = started + args.timeout
        node = None
        while time.perf_counter() < deadline:
            children.check()
            if node is None:
                tree = ipc(env['SWAYSOCK'])
                node = desktop_node(tree, app.pid)
                if node:
                    sample['first_mapped_ms'] = (time.perf_counter() - started) * 1000
                    (root / 'mapped-node.json').write_text(json.dumps(node, indent=2) + '\n')
            if node and sample['first_captured_pixels_ms'] is None:
                image = capture('first-frame.png')
                if changed_client_pixels(baseline, image, node['rect']):
                    sample['first_captured_pixels_ms'] = (time.perf_counter() - started) * 1000
            state = shared.navigation(root / 'state')
            if shared.focused_panel(state) and sample['focused_panel_ms'] is None:
                sample['focused_panel_ms'] = (time.perf_counter() - started) * 1000
                (root / 'first-navigation.json').write_text(json.dumps(state, indent=2) + '\n')
            if sample['first_captured_pixels_ms'] is not None and sample['focused_panel_ms'] is not None:
                break
            time.sleep(args.poll_ms / 1000)
        else:
            (root / 'last-tree.json').write_text(json.dumps(tree, indent=2) + '\n')
            raise TimeoutError('No captured native client pixels and focused panel')
        if args.verify_input:
            ipc(env['SWAYSOCK'], 0, f'[con_id={node["id"]}] focus')
            key('-k', 'Escape')
            time.sleep(.2)
            panel = shared.focused_panel(shared.navigation(root / 'state'))
            if panel and panel.get('session') == 'desktop://changelog':
                key('-M', 'ctrl', '-M', 'shift', '-k', 'w', '-m', 'shift', '-m', 'ctrl')
            wait(lambda: (p := shared.focused_panel(shared.navigation(root / 'state'))) and
                 p.get('session', '').startswith('session_') and p.get('history_loaded'),
                 'connected editable panel')
            # Input is an eventual correctness check, outside startup endpoints.
            # Give the new virtual keyboard enter/keymap time to reach GPUI.
            time.sleep(.5)
            command(['wtype', '-s', '500', '-d', '30', shared.DRAFT])

            def typed_pixels():
                capture('native-input.png')
                text = command(['tesseract', str(root / 'native-input.png'), 'stdout', '--psm', '11'],
                               capture_output=True, text=True).stdout
                (root / 'native-input-ocr.txt').write_text(text)
                return shared.DRAFT.lower() in ' '.join(text.lower().split())

            wait(typed_pixels, 'native wtype pixels')
            sample['input_verified'] = True
            sample['launch_to_input_verified_ms'] = (time.perf_counter() - started) * 1000
        capture('final.png')
        diagnostics = root / 'logs/jcode-desktop/jcode-desktop.log'
        lines = diagnostics.read_text().splitlines() if diagnostics.exists() else []
        sample['graphics'] = next((line for line in lines if 'desktop graphics:' in line), None)
        sample['startup_trace'] = [line for line in lines if 'desktop startup' in line or 'gpui_pipeline_timing' in line]
        sample['hardware_verified'] = 'is_software_emulated: false' in (sample['graphics'] or '')
        if args.require_hardware and not sample['hardware_verified']:
            raise RuntimeError('Hardware rendering not proven: ' + str(sample['graphics']))
        return sample
    except Exception as error:
        sample['error'] = str(error)
        raise
    finally:
        children.close()
        diagnostics = root / 'logs/jcode-desktop/jcode-desktop.log'
        lines = diagnostics.read_text().splitlines() if diagnostics.exists() else []
        sample['graphics'] = next((line for line in lines if 'desktop graphics:' in line), None)
        sample['gpu_device_lost'] = any('device lost' in line for line in lines)
        sample['startup_trace'] = [line for line in lines if 'desktop startup' in line or 'gpui_pipeline_timing' in line]
        (root / 'sample.json').write_text(json.dumps(sample, indent=2) + '\n')


def main():
    def terminate(signum, frame):
        raise KeyboardInterrupt(f'terminated by signal {signum}')

    signal.signal(signal.SIGTERM, terminate)
    repo = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--binary', type=Path, default=repo / 'target/release/jcode-desktop')
    parser.add_argument('--jcode', type=Path)
    parser.add_argument('--sway-prefix', type=Path, default=repo / 'target/headless-sway-tools')
    parser.add_argument('--compositor-renderer', choices=('pixman', 'gles2'), default='pixman')
    parser.add_argument('--render-node', type=Path, help='optional private Sway render device, e.g. /dev/dri/renderD128')
    parser.add_argument('--driver', type=Path, default=Path('/usr/share/vulkan/icd.d/lvp_icd.json'))
    parser.add_argument('--samples', type=int, default=3)
    parser.add_argument('--timeout', type=float, default=60)
    parser.add_argument('--poll-ms', type=float, default=2)
    parser.add_argument('--single-panel', action='store_true')
    parser.add_argument('--software-presentation', action='store_true', help='MESA_VK_WSI_DEBUG=sw for GPU render/software presentation')
    parser.add_argument('--require-hardware', action='store_true')
    parser.add_argument('--verify-input', action=argparse.BooleanOptionalAction, default=True)
    for flag in FLAGS:
        parser.add_argument('--' + flag.lower().replace('_', '-'), choices=('0', '1'),
                            help=f'explicit {flag} A/B value (default: unset)')
    args = parser.parse_args()
    if args.samples < 1 or not all(math.isfinite(x) and x > 0 for x in (args.timeout, args.poll_ms)):
        parser.error('samples, timeout and poll-ms must be positive and finite')
    args.sway_prefix = args.sway_prefix.resolve(strict=True)
    tool_path = str(args.sway_prefix / 'usr/bin') + ':/usr/bin:/bin'
    for tool in ['sway', 'grim'] + (['wtype', 'tesseract'] if args.verify_input else []):
        if not shutil.which(tool, path=tool_path):
            parser.error('missing executable: ' + tool)
    binary, driver = args.binary.resolve(strict=True), args.driver.resolve(strict=True)
    companion = args.jcode or shutil.which('jcode')
    if not companion:
        parser.error('missing jcode companion')
    jcode = Path(companion).resolve(strict=True)
    root = args.output.resolve()
    if len(str(root / f'sample-{args.samples - 1:03}/runtime/daemon.sock').encode()) >= 100:
        parser.error('output path too long for private Unix sockets')
    root.mkdir(parents=True, exist_ok=False, mode=0o700)
    (root / 'bin').mkdir(mode=0o700)
    launch_binary = root / 'bin/jcode-desktop'
    shutil.copy2(binary, launch_binary)
    result = dict(binary=str(binary), binary_sha256=shared.sha256(launch_binary),
                  companion=str(jcode), companion_sha256=shared.sha256(jcode), samples=[],
                  compositor=f'private headless Sway, {args.compositor_renderer} composition, no physical display',
                  compositor_render_node=str(args.render_node) if args.render_node else None,
                  driver=str(driver), software_presentation=args.software_presentation,
                  flags={flag: getattr(args, flag.lower()) for flag in FLAGS},
                  cache_policy='fresh application caches, retained OS page cache',
                  companion_prestarted=False, hot_reload=False, poll_ms=args.poll_ms,
                  endpoint='IPC mapped toplevel then changed client pixels captured by grim; upper bounds, not presentation timestamps',
                  input_note='wtype and screenshot OCR verification, not input latency')
    try:
        for index in range(args.samples):
            sample = run_sample(root / f'sample-{index:03}', index, args, launch_binary, jcode, driver)
            result['samples'].append(sample)
            (root / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
            print(json.dumps(sample), flush=True)
        result['summary'] = {key: shared.summary([s[key] for s in result['samples']])
                             for key in ('first_mapped_ms', 'first_captured_pixels_ms', 'focused_panel_ms')}
    except Exception as error:
        result['error'] = str(error)
        raise
    finally:
        (root / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result['summary'], indent=2))


if __name__ == '__main__':
    main()
