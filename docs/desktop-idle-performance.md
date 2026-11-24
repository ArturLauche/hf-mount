# Desktop idle and filesystem performance

## Results

Baseline `4919e3e` and the optimized checkout ran on the same Debian 12 x86-64 VM with eight Skylake vCPUs, Rust 1.95.0 and Node 24.22.3. Wine 11 ran the Windows GNU debug executables. The frontend comparisons used production Vite bundles and Vitest with a simulated clock.

| Workload | Baseline | Optimized |
| --- | ---: | ---: |
| Overview requests, visible, 10 minutes | 300 | 76 |
| Overview requests, hidden, 10 minutes | 60 | 0 |
| Visible Activity event requests, no hosts, 10 minutes | 200 | 0 |
| Visible Activity event requests, three drives, 10 minutes | 200 | 67 |
| Status-context consumer renders, stable visible drives, 10 minutes | 300 | 0 |
| Initial JavaScript entry, gzip (Vite kB) | 129.34 | 121.55 |
| Empty supervisor CPU time, 90 seconds without clients | 30 ms | below 10 ms resolution |
| Empty supervisor main-thread context switches, same interval | 111 | 0 |
| Supervisor under passive status reads | running at 610 s | exited successfully at 600.45 s |

The frontend tests supplied freshly decoded equivalent snapshots and excluded initial load and visibility transitions. React Profiler also recorded zero commits across the complete three-drive screen for 60 visible seconds and 600 hidden seconds. The old tray timer scheduled another 120 full status reads per ten minutes by inspection. The new tray has no recurring status timer.

The supervisor experiments used separate Wine prefixes, real CLI clients and a real control pipe. Linux process counters sampled the Windows executable. Main-thread context switches are a proxy for wakeups. Supervisor RSS was approximately 65.7 MiB for both versions at 90 seconds. A separate optimized tray-only process answered a window message at both ends of a 90-second interval. It accumulated zero measurable CPU time and zero main-thread context switches, retained ten threads and had zero WebView2 processes at both samples. Its private resident memory went from 77,976 to 77,972 KiB.

A second, 60-second syscall trace covered every thread of the packaged tray app and an empty supervisor under Wine. The tray app made zero observed file, network, read or write calls. One supervisor worker exited, with calls to Wine pipes and the Linux runtime loader. Neither process made an observed network call during the interval. Native Windows private bytes, ETW wakeups, long-term growth, network traffic and startup/restore latency still need measurement.

## Where the work went

| Scenario | Behavior and evidence |
| --- | --- |
| A. Visible, empty | One scheduler polls for external CLI/foreground changes, backing off from 2 to 8 seconds. Unchanged native snapshots return a revision match. A failed read clears obsolete drive data. |
| B. Hidden, empty | Native visibility and document visibility suspend React polling. Cold and repeated `--minimized` launches keep WebView2 unopened. The tray queries live hosts on hover. A manual open or restore immediately requests current state. |
| C. Hidden, mounted | The same fixture with three drives had zero hidden status and Activity timers. The independent host still serves files, uploads changes and follows the configured remote-update policy. |
| D. Desktop closed, mounted | The host waits for WinFsp's dispatcher callback or a lifecycle change. Mounted, starting, stopping and recovering drives prevent idle shutdown. Registry tests exercise loss during teardown and a racing request for the same letter. |
| E. Visible, multiple drives | Status rediscovers hosts each time and reuses verified same-user pipes while visible. Up to four host reads run concurrently. The tray uses the window's result. Activity requests events directly. |
| F. Filesystem workload | Cached range planning copies fetch information for intersecting terms. Exact dirty-file counts are reused across idle queries. The upload path still enumerates the files it must flush. Offline measurements are below. |
| G. Network unavailable or resumed | A cancelled reconstruction fetch releases waiting readers. Failure and retry tests exercise cache misses, HTTP backoff and upload timeouts. Restore starts a fresh read. System suspend and live link loss during a mounted workload require native testing. |

The frontend previously committed newly decoded data on every overview poll. Available native visibility now determines when to read, and equivalent snapshots skip the state update. The Activity page avoids querying hosts when the overview has found none. [React status](../desktop/src/lib/state.tsx) · [Scheduler](../desktop/src/hooks/useVisiblePolling.ts) · [Activity](../desktop/src/pages/Activity.tsx) · [Window lifecycle](../desktop/src-tauri/src/window.rs)

The tray previously ran its own discovery and statistics pass. While hidden it reads once when the pointer enters the icon. Later changes appear on the next hover or window open. The Windows tray dependency checks for the pointer leaving every 15 ms while it is over the icon. A backport stops that timer as soon as the pointer leaves. [Tray](../desktop/src-tauri/src/tray.rs) · [Upstream fix](https://github.com/tauri-apps/tray-icon/pull/393)

Passive Hello, List and Activity requests previously renewed the idle deadline. The old five-second health loop also generated recovery-journal snapshots for healthy mounts. The monitor now wakes for lifecycle changes, driver loss and retries of failed journal writes. [Registry](../src/manager/registry.rs) · [Windows host](../src/windows/host.rs)

## Windows status collection

Median of five rounds of 20 reads per build, using Windows executables in the same Wine prefix. The overview timings cover live host discovery and construction of the Rust overview. Tauri serialization, WebView delivery and rendering are outside this fixture.

| Mean time per read | Baseline | Optimized |
| --- | ---: | ---: |
| Overview, no supervisor | 13.335 ms | 1.246 ms |
| Overview, empty supervisor | 16.575 ms | 2.148 ms |
| Free-letter enumeration, no supervisor | 12.545 ms | 1.106 ms |
| Host list, empty supervisor | 3.501 ms | 0.846 ms |
| Host list, no supervisor | 0.108 ms | 0.137 ms |

Drive-letter enumeration dominated the baseline. One logical-drive mask now serves all 23 candidates, with DOS-device mappings checked for collisions. With a supervisor present, the visible reader reuses its authenticated pipe and requests a fresh list. The absent-driver probe took about 0.03 ms on this host. Native multi-host discovery and detection of an installed WinFsp driver still need profiling. [Drive letters](../src/windows/drives.rs) · [Pipe reader](../src/windows/client.rs) · [Overview command](../desktop/src-tauri/src/commands.rs)

A test DLL with a missing WinFsp export reproduced a loader reference leak. In 201 failed probes the baseline kept the DLL loaded until process exit. The optimized executable explicitly unloaded it after each failed probe. [WinFsp loader](../src/windows/winfsp.rs)

## Shared engine measurements

Median elapsed time across five local rounds. The status and log fixtures used Cargo's test profile with the hf-mount package at optimization level 2. The range fixture compiled the production planning function with `rustc -O`.

| Work in one round | Baseline | Optimized |
| --- | ---: | ---: |
| 100 exact dirty counts, 50,000 inodes, 50 dirty | 43.815 ms | 0.000638 ms |
| 10 tails of 400 lines from an 8 MiB log | 68.615 ms | 8.221 ms |
| 2,000 random 4 KiB plans across 4,096 terms and 1,024 xorbs | 47.642 ms | 4.210 ms |

The first optimized dirty-count round included a full scan and took 1.645 ms. Mutable inode access invalidates the count. Log tailing seeks backwards from EOF and redacts the selected lines. Range planning measures local CPU time. Mounted throughput and network latency were unmeasured. [Inodes](../src/virtual_fs/inode.rs) · [Log reader](../src/manager/logging.rs) · [Xet cache](../src/cached_xet_client.rs)

## Validation and reproduction

- Linux FUSE/NFS library suite passed 600 tests with five opt-in tests skipped. Local directory-paging and poll-reconciliation benchmarks passed separately. One concurrent suite run exceeded the existing quota test's outer timeout. It passed in isolation and on the full rerun with three test threads.
- All 38 Vitest tests, TypeScript, ESLint, production frontend build, Rust formatting and Linux/Windows-target Clippy passed. Under Wine, 12 registry tests, a named-pipe reuse/restart test and a desktop overview revision test passed. Wine returned Windows error 66 when enumerating the pipe namespace, so foreground pipe enumeration still needs the native Windows test.
- The x64 Windows desktop and CLI cross-built. The NSIS archive contained the expected desktop, CLI, WebView2 loader, licenses and SHA-256-pinned WinFsp MSI. The installer smoke test covers cold tray launch, a second minimized launch and opening the first WebView. This host lacks a WinFsp kernel, a FUSE device and privileged mounts. The mounted benchmark harnesses skipped for unavailable credentials and fio. Native Windows installation, MSI packaging and macOS execution remain unverified.

```sh
cd desktop && npm test -- --run src/test/idle-measure.test.tsx
```

```sh
cargo test --release --test status_bench -- --ignored --nocapture
cargo test --release --lib fragmented_random_range_plans -- --ignored --nocapture
```

On Windows, run the `windows_status` benchmark with and without an empty supervisor for component timings. The process measurement script accepts the desktop and supervisor PIDs and `-Seconds 600`. It reports CPU, private bytes, working set, threads and handles for each process group, WebView2 descendants and the combined total. [Status benchmarks](../tests/status_bench.rs) · [Windows measurement script](../scripts/windows/measure-idle.ps1)

## Remaining work, in priority order

1. Capture native Windows ETW and process counters across cold tray, hover/leave, warm hide/restore and an overnight idle session. The first open now pays WebView2 creation cost. A previously opened window retains its form state and requests the low WebView2 memory target while hidden.
2. Profile pipe enumeration, host serialization, drive-letter lookup and statistics collection with several real foreground and supervisor drives. The visible discovery window is up to eight seconds plus request time. Activity can take nine seconds plus request time once stable.
3. Measure mounted random/sequential reads, writes, reconnects and upload drain on WinFsp, FUSE and NFS. Compare reader lock time and cache misses under representative network latency before changing prefetch, flush timing or remote polling.
