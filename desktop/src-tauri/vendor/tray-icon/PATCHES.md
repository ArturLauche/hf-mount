This directory contains the published sources of tray-icon 0.25.1 from crates.io. The only source change is the Windows tray leave-timer fix from [upstream PR 393](https://github.com/tauri-apps/tray-icon/pull/393), commit [23ef3d2](https://github.com/tauri-apps/tray-icon/commit/23ef3d2fe2aff76cc4966099d560ede59191e000). The original licenses are included here.

The fix stops a 15 ms timer that could keep waking an idle tray application after the pointer left the icon. Remove this directory and the desktop manifest's Cargo patch when a compatible release includes the fix.
