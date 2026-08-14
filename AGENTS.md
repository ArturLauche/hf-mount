# hf-mount — agent notes

Fork of huggingface/hf-mount. Focus area: the Windows GUI (`src/bin/hf-mount-gui/`,
egui/eframe, NFS backend only on Windows).

## Build & test

- MSRV: Rust 1.95 (pinned in CI via dtolnay/rust-toolchain@1.95.0; eframe 0.36 requires 1.95).
- rustfmt: pinned nightly (`nightly-2026-04-22`), `cargo +nightly-2026-04-22 fmt`.
- GUI build: `cargo build --no-default-features --features nfs,gui --bin hf-mount-gui`.
- GUI tests: `cargo test --no-default-features --features nfs,gui --bin hf-mount-gui`.
- CI clippy runs with `-D warnings` on: no features, nfs, fuse, fuse+nfs, and
  `--no-default-features --features nfs,gui --bins --tests`.
- fuse builds need `libfuse3-dev`; everything needs `pkg-config` + `libssl-dev` on Linux.
- Windows cross-check from Linux: `rustup target add x86_64-pc-windows-gnu` +
  `gcc-mingw-w64-x86-64`, then `cargo check --target x86_64-pc-windows-gnu ...`.
  (msvc target can't build blake3/aws-lc-sys locally.)
- Visual verification: `Xvfb :99 & DISPLAY=:99 ./target/debug/hf-mount-gui`, screenshot
  with `import -window root`.

## GUI architecture

- `main.rs` doubles as CLI (`--check-setup`, `--background-worker`).
- `worker.rs`: detached background worker with JSON status-file IPC and a poller
  thread; careful PID-reuse and NFS-wedge guards — do not run blocking probes on
  the UI thread or before the first frame.
- `app.rs`: `SharedStatus` has a `revision` counter; the UI keeps a frame-local
  `status_cache` and re-clones only when the revision moves. Control paths that
  must see same-frame writes use `live_state()`, drawing uses `current_status()`.
- `profile.rs`: saved profile (JSON in per-user config dir). New fields MUST get
  `#[serde(default...)]` mirroring the CLI defaults in `setup.rs::MountOptions`.
  Inline HF tokens are never persisted.
- egui 0.36 API: `egui::Panel::{left,bottom}` (no more SidePanel/TopBottomPanel),
  `eframe::App::ui(&mut self, ui, frame)` instead of `update(ctx, ...)`,
  `CornerRadius` (u8), integer `Margin`, `Frame::new()`, per-theme styles via
  `ctx.set_style_of(Theme::Dark, style)`, float literals in `Stroke::new` need
  `_f32` suffix.

## Gotchas

- When upstream adds fields to `MountOptions` (`src/setup.rs`), the GUI's
  `profile_mount_options` in `src/bin/hf-mount-gui/profile.rs` must be updated
  or the gui feature stops compiling (this broke the fork once).
- Windows workflows: `.github/workflows/windows-build.yml` and `release.yml`
  build `hf-mount-nfs.exe` + `hf-mount-gui.exe` with `RUSTFLAGS=-C target-feature=+crt-static`.
