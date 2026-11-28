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

## Shipped optimization: parallel Linux/Vulkan pipelines

GPUI now constructs its nine independent render pipelines using at most four
workers (including the calling thread), bounded by available CPU parallelism.
Descriptors, ordering of returned pipelines, blending, sample counts, and shader
validation are unchanged. All jobs complete before the window uses the results.
Linux/Vulkan is enabled by default. Other backends and Web remain serial, and
`GPUI_PIPELINE_WORKERS=1` opts out. The fix is upstream in GPUI, pinned by Desktop,
not a Desktop-only renderer workaround.

Ten interleaved pairs used **the exact same release executable**, changing only
`GPUI_PIPELINE_WORKERS=1` versus `4`, with fresh per-launch application/shader
caches and verified Intel Arc B390 Vulkan rendering:

| Metric | Serial | Four workers |
| --- | ---: | ---: |
| Launch to rendered focused panel, median | 286.8 ms | 212.7 ms |
| Launch to rendered focused panel, p95 | 369.0 ms | 281.4 ms |
| Render-pipeline construction, median | 112.5 ms | 48.1 ms |

The median startup measurement fell **25.8%**. Parallel won 8 of 10 paired
launches, with median paired reduction 76.9 ms. Native typed text was verified
in the first serial and parallel launch, and the accelerated screenshot was
visually inspected. This is private X11 with GPU rendering and software
presentation, not a measurement of the active Wayland compositor.

Evidence: `target/startup-gpu-pairs/results.json`, binary SHA-256
`2679cf798f7c9f129c574c2fa3c3fb3da7ba69afad571883935a7a763b72e11b`.
The tested candidate had serial default; the final revision changes the default
only for Linux/Vulkan and adds a regression test for that backend policy.
GPUI's 45 tests pass, including bounded concurrency, exactly-once execution,
result ordering, empty/single job handling, panic propagation, and backend policy.
The final default-on release passed the Desktop workspace suite and two fresh
hardware/native-typing checks without a worker override: rendered state at
227.5 ms and 178.5 ms, pipeline creation at 38.5 ms and 34.6 ms. These are
acceptance smoke checks, not additional paired speedup estimates. Evidence:
`target/startup-default-final/results.json` and `target/startup-final-ui.png`.

## Reproduce

```sh
justrust test --workspace
justrust build --release -p jcode-desktop -p jcode-desktop-ui
python3 -m unittest discover -s scripts -p test_profile_startup.py -v
python3 scripts/profile-startup.py target/startup-measure --samples 8 --trace-startup
python3 scripts/profile-startup.py target/startup-gpu --samples 8 --single-panel \
  --trace-startup --driver /usr/share/vulkan/icd.d/intel_icd.json \
  --software-presentation --require-hardware
# Compare the same binary with --pipeline-workers 1 and --pipeline-workers 4.
# --trace-startup also collects GPUI shader/pipeline phase timings.
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
