# ObsLite (v0.5 Stable)

An ultra-lightweight, high-stability RTMP/RTMPS broadcast studio client engineered in Rust for low-end, mid-range, and high-performance Windows PCs.

---

## What's New in v0.5 Stable

1. **True Device-Based Hardware Pre-Flight & Live Throughput Benchmark (`src/detect.rs`)**
   * Enumerates all hardware GPUs (evaluating both Dedicated VRAM and Shared GPU System Memory for modern APUs/dGPUs), CPU physical/logical cores, RAM, and Media Foundation encoders.
   * Executes a real-time memory & color-conversion throughput benchmark on startup to compute a **0–100 Hardware Capability Score** and assign accurate device tiers (`High-Performance`, `Mid-Range Ready`, `Standard Ready`, `Entry Tier`) plus manual quality overrides (`Auto`, `1080p60`, `1080p30`, `720p60`, `720p30`, `540p30`).
2. **Rock-Solid 30–60 FPS Capture & Wall-Clock RTMP Synchronization (`src/capture.rs`, `src/rtmp.rs`)**
   * **Single-Pass LUT-Accelerated BGRA-to-I420 Pipeline**: Precomputed coordinate lookup tables and single-pass planar YUV420 conversion eliminate intermediate full-frame copies and integer division bottlenecks.
   * **CameraVideoRealTime Fast H.264 Encoding**: Eliminates the 15 -> 2 FPS motion stalls on games and dynamic video.
   * **Real Monotonic Wall-Clock Timestamps & 48kHz AAC Clock**: Prevents RTMP buffer underruns, 30-second loading stalls, and 4x catch-up playback speedups.
   * **Bidirectional RTMP Control Polling**: Automatically responds to ingest server `PingRequest` messages (`PingResponse`) and enforces socket write timeouts.
3. **Isolated 3-Stage Application/Game Window Capture (`src/capture.rs`, `src/win_enum.rs`)**
   * Captures target windows directly by `HWND` via DWM `PrintWindow(PW_RENDERFULLCONTENT)` and GDI client-surface `BitBlt` with cropped DXGI fallback.
   * **Never leaks or falls back to Whole Desktop** when Window Mode is selected, even if the game is occluded by other windows or minimized on Alt-Tab.
4. **OLED Dark Broadcast Studio GUI (`/ui-ux-pro-max` Design System) & Apply-Lock Workflow (`src/ui.rs`)**
   * Deep midnight OLED studio palette (`#0C0E18` / `#16192B`) with high-contrast telemetry cards, real-time Audio Mixer VU meters, and a live bitrate sparkline.
   * Explicit **Apply Source & Destination** lock: locks capture source, window target, quality preset, and ingest destination before streaming, and prevents accidental edits while live on air.

---

## Build & Run

```bash
# Run unit tests
cargo test

# Launch release build
cargo run --release
```
