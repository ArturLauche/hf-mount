This directory contains the published sources of tray-icon 0.25.1 from crates.io. The original licenses are included here.

- [Upstream PR 393](https://github.com/tauri-apps/tray-icon/pull/393) stops the 15 ms Windows leave timer when the pointer leaves. The local patch arms it on the first hover and keeps it active only while hovering.
- Windows also retries failed registration with backoff, holds window state safely through callbacks, uses GUIDs and bounded UTF-16 for tooltips, and constructs a word-aligned icon mask.
- The remaining local changes fix builder IDs, validate and finish GTK PNGs before replacing the current icon, preserve macOS icon templates, and shut down the KSNI service if its watcher cannot start.

Remove this directory and the desktop manifest's Cargo patch when a compatible release provides the Windows behavior described above.
