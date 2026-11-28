# Desktop startup profiling (2026-11-28 UTC)

## Attribution and rejected experiment

`JCODE_DESKTOP_TRACE_STARTUP=1` emits process-relative instance/platform/window/UI
checkpoints to the normal diagnostics log. These are local timings, not telemetry.
Initial profiling found sub-millisecond UI activation after opening the native
window. Graphics setup and the first frame dominated cold launch.

An experiment overlapped font matching with graphics initialization. Although
30–60 ms of font work moved off the first frame, eight interleaved native Intel
GPU pairs did not demonstrate an end-to-end improvement: render-state medians
were 285.7 ms before and 306.2 ms after, with paired median delta -3.1 ms and
four wins/four losses. The font-prewarm experiment was removed rather than
shipping a change supported only by an isolated work-duration measurement.
Evidence: `target/startup-intel-verified/results.json`.

## Reproduce

```sh
justrust test --workspace
justrust build --release -p jcode-desktop -p jcode-desktop-ui
python3 -m unittest discover -s scripts -p test_profile_startup.py -v
python3 scripts/profile-startup.py target/startup-measure --samples 8 --trace-startup
python3 scripts/profile-startup.py target/startup-gpu --samples 8 --single-panel \
  --trace-startup --driver /usr/share/vulkan/icd.d/intel_icd.json \
  --software-presentation --require-hardware
python3 scripts/screenshot.py target/startup-ui.png --no-build \
  --binary target/release/jcode-desktop --transcript empty
```

The profiler snapshots the executable, isolates home, configuration, credentials,
sockets, application/shader caches, and display, and pins the companion binary.
It launches a private Xvfb/Openbox pair, measures first visible window and rendered
navigation state independently, and verifies native typing without submitting
anything. The default includes OCR or clipboard verification after timing ends.
The endpoint is rendered diagnostic state, not physical input-to-photon.
OS page caches are not flushed. Use interleaved before/after runs when comparing
small gains on a busy machine.

Selecting Intel's ICD alone is insufficient on Xvfb: Vulkan can fall back to
software OpenGL. `--software-presentation` asks Mesa to render on the GPU while
copying the swapchain to X11 in software. `--require-hardware` checks the actual
Desktop GPU diagnostic and rejects missing or software-rendered evidence. This
still does not reproduce the user's native Wayland presentation path.

## Host delivery constraint

The running host's Ctrl+R rebuild succeeded, but activation was safely rejected
because its existing `jcode-desktop-api` shared-global identity differs from
the checkout. The identity guard was not bypassed and active windows were not
restarted or discarded. Startup changes in the host require the next normal
full launch regardless of UI hot reload.

Raw local evidence is in `target/startup-*`; benchmark JSON records binary hashes,
sample timings, selected graphics device, and native-input proof independently.
