#!/usr/bin/env python3
"""Benchmark process launch to rendered startup state on a private Xvfb display.

Uses an existing binary, never builds or hot reloads. Each sample has fresh homes,
cache, sockets, X server and a cold Jcode companion. A private binary snapshot
prevents sibling companion discovery and concurrent rebuilds changing samples.
OS page caches are NOT flushed.
The primary endpoint is the application's navigation diagnostic, not physical
input-to-photon. First visible X11 window and optional native typing are separate
endpoints. No prompts are submitted and no real credentials are inherited.

Example: python3 scripts/profile-startup.py target/startup-baseline --samples 8
Intel rendering on private Xvfb (software presentation, not a live display):
  python3 scripts/profile-startup.py target/startup-intel --single-panel \\
    --driver /usr/share/vulkan/icd.d/intel_icd.json --software-presentation \\
    --require-hardware --trace-startup
Without software presentation, an Intel ICD can silently fall back to OpenGL
llvmpipe because Xvfb lacks DRI3. Always inspect the saved graphics diagnostics.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import select
import shlex
import shutil
import signal
import statistics
import subprocess
import threading
import time

from screenshot import isolated_env


DRAFT = 'Startup benchmark native typing works'


def navigation(path):
    """Ignore missing/partially written diagnostics until the next poll."""
    try:
        value = json.loads(next(line.partition('=')[2] for line in path.read_text().splitlines()
                                if line.startswith('navigation=')))
        return value if isinstance(value, dict) else {}
    except (OSError, StopIteration, ValueError):
        return {}


def focused_panel(state):
    for row in state.get('rows', []):
        for panel in row.get('panels', []):
            if panel.get('focused') and panel.get('slot') == state.get('keyboard_panel'):
                return panel
    return None


def summary(values):
    """Nearest-rank p95, including the first sample (no hidden warmup)."""
    ordered = sorted(values)
    return dict(count=len(ordered), min_ms=ordered[0], median_ms=statistics.median(ordered),
                p95_ms=ordered[math.ceil(.95 * len(ordered)) - 1], max_ms=ordered[-1])


def sample_env(root, jcode, driver, timeout, trace_startup=False, software_presentation=False):
    for name in ('home', 'runtime', 'config', 'cache', 'data', 'jcode', 'tmp', 'shims'):
        (root / name).mkdir(mode=0o700)
    # Keep real startup behavior, but select a credential-free provider for the
    # private companion. No model request is made. Never expose the user's PATH.
    shim = root / 'shims/jcode'
    shim.write_text('#!/bin/sh\nset -eu\n'
                    'if [ "${1:-}" = serve ]; then\n  shift\n'
                    f'  exec {shlex.quote(str(jcode))} --no-update --no-selfdev --provider jcode serve "$@"\n'
                    'fi\n'
                    f'exec {shlex.quote(str(jcode))} --no-update --no-selfdev "$@"\n')
    shim.chmod(0o700)
    (root / 'desktop.toml').write_text(
        '[workspace]\ncoaching_hints = false\naccount_sign_in_handled = true\n')
    env = isolated_env(root)
    env.pop('JCODE_DESKTOP_SCREENSHOT', None)
    if not driver.name.startswith('lvp'):
        env.pop('LIBGL_ALWAYS_SOFTWARE', None)
    if trace_startup:
        env['JCODE_DESKTOP_TRACE_STARTUP'] = '1'
    if software_presentation:
        # Xvfb has no DRI3. Mesa can render on the selected GPU and copy its
        # swapchain to X11 in software. The graphics diagnostic still identifies
        # the actual rendering GPU, so pair this with --require-hardware.
        env['MESA_VK_WSI_DEBUG'] = 'sw'
    env.update(PATH=str(root / 'shims') + ':/usr/bin:/bin',
               JCODE_NO_TELEMETRY='1', JCODE_RUNTIME_DIR=str(root / 'runtime'),
               JCODE_API_SOCKET=str(root / 'runtime/api.sock'),
               JCODE_SOCKET=str(root / 'runtime/daemon.sock'),
               JCODE_TEMP_SERVER='1', JCODE_SERVER_OWNER_PID=str(os.getpid()),
               JCODE_TEMP_SERVER_IDLE_SECS=str(math.ceil(timeout + 60)),
               JCODE_DESKTOP_CONFIG=str(root / 'desktop.toml'),
               VK_DRIVER_FILES=str(driver), TMPDIR=str(root / 'tmp'))
    return env


def sha256(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


class WindowWatch:
    """Poll independently: xdotool --sync has a coarse internal retry delay."""
    def __init__(self, env, poll_ms):
        self.env, self.poll_ms = env, poll_ms
        self.stop = threading.Event()
        self.window_id, self.seen_at, self.error = None, None, None
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        try:
            while not self.stop.is_set():
                result = subprocess.run(['xdotool', 'search', '--onlyvisible', '--limit', '1',
                                         '--class', '^jcode-desktop$'], env=self.env,
                                        capture_output=True, text=True, timeout=5)
                seen_at = time.perf_counter()
                if result.returncode == 0:
                    window_id = result.stdout.strip()
                    if not window_id.isdigit():
                        raise RuntimeError('Invalid visible window id: ' + window_id)
                    self.seen_at = seen_at
                    self.window_id = window_id
                    return
                if result.returncode != 1:
                    raise RuntimeError('Window observer failed: ' + result.stderr)
                self.stop.wait(self.poll_ms / 1000)
        except Exception as error:
            self.error = error

    def close(self):
        self.stop.set()
        self.thread.join(timeout=6)


class Children:
    def __init__(self, root, env):
        self.root, self.env = root, env
        self.processes, self.logs = [], []

    def launch(self, name, command, **kwargs):
        log = (self.root / (name + '.log')).open('w')
        self.logs.append(log)
        process = subprocess.Popen(command, cwd=self.root, env=self.env, stderr=log,
                                   start_new_session=True, **{'stdout': log, **kwargs})
        self.processes.append((name, process))
        return process

    def check(self):
        for name, process in self.processes:
            if process.poll() is not None:
                raise RuntimeError(f'{name} exited ({process.returncode}), see {self.root}')

    def close(self):
        # Kill the entire owned process group even when its leader has exited.
        # This also reaps a companion/bridge inherited from the desktop process.
        for _, process in reversed(self.processes):
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        for _, process in reversed(self.processes):
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            if process.stdout:
                process.stdout.close()
        for log in self.logs:
            log.close()


def run_sample(root, index, args, binary, jcode, driver):
    root.mkdir(mode=0o700)
    env = sample_env(root, jcode, driver, args.timeout, args.trace_startup,
                     args.software_presentation)
    children = Children(root, env)
    read_fd, write_fd = os.pipe()
    state_path = root / 'state'
    watcher = None

    def command(argv, **kwargs):
        return subprocess.run(argv, env=env, cwd=root, check=True, timeout=15, **kwargs)

    def key(value):
        command(['xdotool', 'key', '--clearmodifiers', '--delay', '0', value])

    def wait(predicate, label):
        deadline = time.perf_counter() + args.timeout
        while time.perf_counter() < deadline:
            children.check()
            result = predicate()
            if result:
                return result
            time.sleep(args.poll_ms / 1000)
        raise TimeoutError(f'{label}: see {root}')

    try:
        children.launch('xvfb', ['Xvfb', '-displayfd', str(write_fd), '-screen', '0',
                                '1440x1000x24', '-nolisten', 'tcp'], pass_fds=(write_fd,))
        os.close(write_fd)
        write_fd = None
        if not select.select([read_fd], [], [], 15)[0]:
            raise TimeoutError('Xvfb did not allocate a display')
        display = os.read(read_fd, 64).decode().strip()
        if not display.isdigit():
            raise RuntimeError('Invalid Xvfb display: ' + display)
        env['DISPLAY'] = ':' + display
        wm = root / 'openbox.xml'
        wm.write_text('<openbox_config xmlns="http://openbox.org/3.4/rc"><applications>'
                      '<application class="*"><decor>no</decor><maximized>yes</maximized>'
                      '</application></applications></openbox_config>')
        children.launch('wm', ['openbox', '--sm-disable', '--config-file', str(wm)])
        # Explicitly await EWMH readiness outside the measured interval.
        wait(lambda: '_NET_SUPPORTING_WM_CHECK(WINDOW)' in command(
            ['xprop', '-root', '_NET_SUPPORTING_WM_CHECK'], capture_output=True,
            text=True).stdout, 'window manager readiness')
        watcher = WindowWatch(env, args.poll_ms)
        started = time.perf_counter()
        launch_args = [str(binary), '--no-hot-reload']
        if args.single_panel:
            launch_args.append('--single-panel')
        children.launch('desktop', launch_args)
        sample = dict(sample=index, first_window_ms=None, render_state_ms=None,
                      focused_panel_ms=None, input_verified=False)
        first_state = None
        window_id = None
        deadline = started + args.timeout
        while time.perf_counter() < deadline:
            children.check()
            if watcher.error:
                raise watcher.error
            if window_id is None and watcher.window_id is not None:
                window_id = watcher.window_id
                sample['first_window_ms'] = (watcher.seen_at - started) * 1000
            state = navigation(state_path)
            if state.get('rows') and sample['render_state_ms'] is None:
                sample['render_state_ms'] = (time.perf_counter() - started) * 1000
                first_state = state
            panel = focused_panel(state)
            if panel and sample['focused_panel_ms'] is None:
                sample['focused_panel_ms'] = (time.perf_counter() - started) * 1000
                sample['first_focused_session'] = panel.get('session')
            if window_id is not None and sample['focused_panel_ms'] is not None:
                break
            time.sleep(args.poll_ms / 1000)
        else:
            raise TimeoutError(f'No rendered focused panel/window: {root}')
        (root / 'first-navigation.json').write_text(json.dumps(first_state, indent=2) + '\n')
        # Evidence work is deliberately outside all startup timing endpoints.
        if args.verify_input:
            command(['xdotool', 'windowactivate', '--sync', window_id])
            key('Escape')
            time.sleep(.2)
            panel = focused_panel(navigation(state_path))
            if panel and panel.get('session') == 'desktop://changelog':
                key('ctrl+shift+w')
            wait(lambda: (p := focused_panel(navigation(state_path))) and
                 (p.get('session', '').startswith(('startup://draft', 'session_'))),
                 'editable first panel')
            input_started = time.perf_counter()
            command(['xdotool', 'type', '--clearmodifiers', '--delay', '0', DRAFT])
            if shutil.which('xclip'):
                key('ctrl+a')
                key('ctrl+c')
                copied = command(['xclip', '-selection', 'clipboard', '-o'],
                                 capture_output=True, text=True).stdout
                if copied != DRAFT:
                    raise AssertionError('Native clipboard text differs: ' + repr(copied))
                sample['input_proof'] = 'clipboard'
            else:
                # Rendering and OCR overhead are NOT input-to-frame latency.
                def typed_pixels():
                    command(['import', '-window', 'root', str(root / 'native-input.png')])
                    text = command(['tesseract', str(root / 'native-input.png'), 'stdout',
                                    '--psm', '11'], capture_output=True, text=True).stdout
                    return DRAFT.lower() in ' '.join(text.lower().split())
                wait(typed_pixels, 'native typed pixels')
                sample['input_proof'] = 'screenshot-ocr'
            sample['input_verified'] = True
            sample['native_input_check_ms'] = (time.perf_counter() - input_started) * 1000
            sample['launch_to_input_verified_ms'] = (time.perf_counter() - started) * 1000
        command(['import', '-window', 'root', str(root / 'final.png')])
        diagnostics = root / 'logs/jcode-desktop/jcode-desktop.log'
        if diagnostics.exists():
            lines = diagnostics.read_text().splitlines()
            sample['graphics'] = next((line for line in lines if 'desktop graphics:' in line), None)
            sample['startup_trace'] = [line for line in lines if 'desktop startup' in line]
        sample['hardware_verified'] = 'is_software_emulated: false' in (sample.get('graphics') or '')
        if args.require_hardware and not sample['hardware_verified']:
            raise RuntimeError('Hardware rendering required but graphics diagnostic is software or missing: '
                               + str(sample.get('graphics')))
        (root / 'sample.json').write_text(json.dumps(sample, indent=2) + '\n')
        return sample
    finally:
        if watcher:
            watcher.close()
        os.close(read_fd)
        if write_fd is not None:
            os.close(write_fd)
        children.close()


def main():
    repo = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path, help='new directory for JSON, screenshots and private logs')
    parser.add_argument('--binary', type=Path, default=repo / 'target/release/jcode-desktop')
    parser.add_argument('--jcode', type=Path, help='companion binary (default: jcode on PATH)')
    parser.add_argument('--driver', type=Path,
                        help='Vulkan ICD JSON (default: Mesa lavapipe, use intel_icd.json for hardware)')
    parser.add_argument('--single-panel', action='store_true', help='measure first single-panel host window')
    parser.add_argument('--software-presentation', action='store_true',
                        help='Mesa GPU rendering with software X11 presentation (MESA_VK_WSI_DEBUG=sw)')
    parser.add_argument('--require-hardware', action='store_true',
                        help='fail if app diagnostics report software rendering or omit GPU evidence')
    parser.add_argument('--trace-startup', action='store_true',
                        help='set JCODE_DESKTOP_TRACE_STARTUP=1 and save app checkpoint logs')
    parser.add_argument('--samples', type=int, default=8)
    parser.add_argument('--timeout', type=float, default=60)
    parser.add_argument('--poll-ms', type=float, default=2)
    parser.add_argument('--verify-input', action=argparse.BooleanOptionalAction, default=True,
                        help='verify native typing without submitting (default: enabled)')
    args = parser.parse_args()
    if args.samples < 1 or not all(math.isfinite(x) and x > 0 for x in (args.timeout, args.poll_ms)):
        parser.error('samples, timeout and poll-ms must be positive and finite')
    for name in ('Xvfb', 'openbox', 'xdotool', 'xprop', 'import'):
        if not shutil.which(name):
            parser.error('missing executable: ' + name)
    if args.verify_input and not (shutil.which('xclip') or shutil.which('tesseract')):
        parser.error('native typing verification needs xclip or tesseract')
    drivers = sorted(Path('/usr/share/vulkan/icd.d').glob('lvp_icd*.json'))
    if args.driver:
        driver = args.driver.resolve(strict=True)
    elif drivers:
        driver = drivers[0]
    else:
        parser.error('Mesa lavapipe Vulkan driver is required, or specify --driver')
    binary = args.binary.resolve(strict=True)
    companion = args.jcode or shutil.which('jcode')
    if not companion:
        parser.error('missing jcode companion, use --jcode')
    jcode = Path(companion).resolve(strict=True)
    root = args.output.resolve()
    if len(str(root / f'sample-{args.samples - 1:03}/runtime/daemon.sock').encode()) >= 104:
        parser.error('output path is too long for isolated Unix sockets')
    root.mkdir(parents=True, exist_ok=False, mode=0o700)
    # Desktop prefers a sibling `jcode` over PATH. A private executable snapshot
    # both pins the measured bytes against concurrent builds and ensures --jcode
    # selects our credential-free shim, not an unrelated packaged companion.
    (root / 'bin').mkdir(mode=0o700)
    launch_binary = root / 'bin/jcode-desktop'
    shutil.copy2(binary, launch_binary)
    result = dict(binary=str(binary), launch_binary=str(launch_binary), binary_sha256=sha256(launch_binary),
                  companion=str(jcode), companion_sha256=sha256(jcode),
                  samples=[], poll_ms=args.poll_ms, hot_reload=False, fresh_home_per_sample=True,
                  companion_prestarted=False, driver=str(driver),
                  renderer='Mesa lavapipe' if driver.name.startswith('lvp') else 'Selected Vulkan ICD (see sample graphics)',
                  single_panel=args.single_panel, trace_startup=args.trace_startup,
                  software_presentation=args.software_presentation, require_hardware=args.require_hardware,
                  resolution='1440x1000x24',
                  first_window_observer='Independent xdotool polling, interval plus subprocess overhead',
                  endpoint='First navigation diagnostic with rows, not physical input-to-photon',
                  cache_policy='Fresh application and shader caches per sample, OS page caches retained',
                  input_note='Native verification includes automation/OCR and onboarding dismissal, not latency')
    try:
        for index in range(args.samples):
            sample = run_sample(root / f'sample-{index:03}', index, args, launch_binary, jcode, driver)
            result['samples'].append(sample)
            print(json.dumps(sample), flush=True)
            (root / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
        result['summary'] = {key: summary([s[key] for s in result['samples']])
                             for key in ('first_window_ms', 'render_state_ms', 'focused_panel_ms')}
    except Exception as error:
        result['error'] = str(error)
        raise
    finally:
        (root / 'results.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result['summary'], indent=2), flush=True)


if __name__ == '__main__':
    main()
