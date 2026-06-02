# hf-mount

<img width="1400" height="386" alt="image" src="https://github.com/user-attachments/assets/d68eac8c-4e28-4d2d-93b2-b049da846397" />

Mount [Hugging Face Buckets](https://huggingface.co/docs/hub/storage-buckets) and repos as local filesystems. No download, no copy, no waiting.

```bash
hf-mount start bucket myuser/my-bucket /tmp/data
```

Also works with any model or dataset repo (read-only):

```bash
hf-mount start repo openai/gpt-oss-20b /tmp/gpt-oss
```

Commands will pick up your `HF_TOKEN` from the environment, or you can pass it explicitly with `--hf-token`.

Then use your local folders as usual:
```python
from transformers import AutoModelForCausalLM
model = AutoModelForCausalLM.from_pretrained("/tmp/gpt-oss")  # reads on demand, no download step
```

hf-mount exposes [Hugging Face Buckets](https://huggingface.co/docs/hub/storage-buckets) and [Hub repos](https://huggingface.co) as a local filesystem via FUSE or NFS. Files are fetched lazily on first read, so only the bytes your code actually touches ever hit the network.

Two backends are available:
- **NFS** (recommended) -- works everywhere, no root, no kernel extension
- **FUSE** -- tighter kernel integration, requires root or [macFUSE](https://osxfuse.github.io/) on macOS

Agentic storage: Agents don't require complex APIs or SDKs, they thrive on the filesystem: ls, cat, find, grep, and the power of composable UNIX pipelines.

![hf-mount demo gif](https://huggingface.co/datasets/huggingface/documentation-images/resolve/main/hf-mount/demo.gif)

## Install

### Homebrew (macOS, Linux)

```bash
brew install hf-mount
```

On macOS, this installs the NFS backend only (`hf-mount`, `hf-mount-nfs`). For the FUSE backend on macOS, download the binary manually or build from source — macFUSE is closed-source and not distributable through homebrew-core.

### Manual download

Binaries are available on [GitHub Releases](https://github.com/huggingface/hf-mount/releases):

| Platform | Daemon | NFS | FUSE | GUI |
| --- | --- | --- | --- | --- |
| Linux x86_64 | `hf-mount-x86_64-linux` | `hf-mount-nfs-x86_64-linux` | `hf-mount-fuse-x86_64-linux` | Not available |
| Linux aarch64 | `hf-mount-aarch64-linux` | `hf-mount-nfs-aarch64-linux` | `hf-mount-fuse-aarch64-linux` | Not available |
| macOS Apple Silicon | `hf-mount-arm64-apple-darwin` | `hf-mount-nfs-arm64-apple-darwin` | `hf-mount-fuse-arm64-apple-darwin` | `hf-mount-gui-arm64-apple-darwin.app.zip` or `hf-mount-gui-arm64-apple-darwin` |
| Windows x64 | Not available | `hf-mount-windows-x64.exe` | Not available | `hf-mount-gui-windows-x64.exe` |

For non-release builds, GitHub Actions uploads these artifacts:
- **Windows Build**: `hf-mount-windows-x64` and `hf-mount-gui-windows-x64`
- **macOS GUI Build**: `hf-mount-gui-macos-arm64`

### System dependencies (FUSE only)

On Linux and macOS, the NFS backend has no system dependencies. For FUSE:

**Linux**: `sudo apt-get install -y fuse3` (pre-built binaries only need the runtime; building from source also requires `libfuse3-dev`)

**macOS**: install [macFUSE](https://osxfuse.github.io/) (`brew install macfuse`, requires reboot on first install)

### System dependencies (Windows)

Windows support uses the NFS backend only. FUSE, the `hf-mount start/stop/status` daemon controller, and the FUSE sidecar are Unix-only. The GUI executable uses the same Windows NFS backend and has the same setup requirements as the CLI executable.

Enable Microsoft's Client for NFS feature:

```powershell
# Windows Server
Install-WindowsFeature -Name NFS-Client

# Windows 10 / 11
Enable-WindowsOptionalFeature -Online -FeatureName ServicesForNFS-ClientOnly,ClientForNFS-Infrastructure -All
```

Run `hf-mount-windows-x64.exe` from an Administrator PowerShell or Command Prompt. It binds the local NFS portmapper on `127.0.0.1:111`, which requires Administrator privileges. If a drive mounted from an Administrator shell is not visible in non-admin Explorer or applications, enable linked connections and reboot:

```cmd
reg add HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System /v EnableLinkedConnections /t REG_DWORD /d 1 /f
```

### Build from source

Requires Rust 1.89+.

```bash
# NFS only (Windows builds produce hf-mount-nfs.exe)
cargo build --release --features nfs

# Native GUI (NFS backend; Windows and macOS)
cargo build --release --no-default-features --features nfs,gui --bin hf-mount-gui

# FUSE (requires macFUSE on macOS, fuse3 on Linux; not available on Windows)
cargo build --release --features fuse

# All backends
cargo build --release --features fuse,nfs
```

Binaries: `target/release/hf-mount`, `target/release/hf-mount-nfs`, `target/release/hf-mount-fuse`, and `target/release/hf-mount-gui` when the `gui` feature is enabled. On Windows, use `target/release/hf-mount-nfs.exe` for the CLI or `target/release/hf-mount-gui.exe` for the GUI; release builds publish them as `hf-mount-windows-x64.exe` and `hf-mount-gui-windows-x64.exe` with a statically linked C runtime.

## Best for / Not for

**Best for:**
- Loading models and datasets without downloading the full repo
- Browsing repo contents (`ls`, `cat`, `find`) without cloning
- Read-heavy ML workloads (training, inference, evaluation)
- Environments where disk space is limited

**Not for:**
- General-purpose networked filesystem (no multi-writer support, no cross-node file locking)
- Latency-sensitive random I/O (first reads require network round-trips)
- Workloads that need strong consistency (files can be stale for up to 10 s)
- Heavy concurrent writes from multiple mounts (last writer wins, no conflict detection)
- Editing files with text editors in default (streaming) mode (use `--advanced-writes`)

Advisory file locks (`flock`, `fcntl` POSIX record locks) are supported locally on a single mount on both backends — enough for Python `filelock`, `huggingface_hub`, `datasets`, and similar cache-coordination use cases within one machine. They are not coordinated across multiple clients.

See [Consistency model](#consistency-model) for details.

## Usage

### GUI app (Windows and macOS)

Download the GUI binary from [GitHub Releases](https://github.com/huggingface/hf-mount/releases):

- Windows x64: `hf-mount-gui-windows-x64.exe`
- macOS Apple Silicon app bundle: `hf-mount-gui-arm64-apple-darwin.app.zip`
- macOS Apple Silicon raw binary: `hf-mount-gui-arm64-apple-darwin`

Windows users must enable Client for NFS and run the GUI from an Administrator session. The GUI includes a **Check setup** action that validates elevation, the Windows NFS client tools, port `111`, and the mount target before starting. If it was launched without elevation, use **Restart as admin** in the GUI and approve the Windows UAC prompt. Fill in the repo or bucket ID, mount point, optional token, then press **Start mount**. Press **Stop mount** to unmount.

Enable **Background** before starting if the mount should keep running after the GUI window is closed. Enable **Start at login** to register the saved GUI mount profile for autostart. On Windows this creates a user logon Scheduled Task with highest privileges; on macOS it writes a user LaunchAgent; on Linux desktops it writes an XDG autostart entry. The GUI saves the mount profile under the user config directory and writes background status/log files there as well.

If the GUI is opened while a background worker is already running, it reconnects to the saved worker status so you can stop or open the active mount from the new window.

You can run the same setup validation without opening the window:

```powershell
.\hf-mount-gui-windows-x64.exe --check-setup Z:
```

Use an unused drive letter such as `Z:` for the most reliable Windows GUI mount. If `Z:` already exists, choose another unused letter such as `Y:` or `X:`. Directory targets depend on the Windows NFS client and are not as reliable.

On macOS, unzip the `.app.zip` and open `hf-mount.app`. If you use the raw binary instead, make it executable before first run:

```bash
chmod +x ./hf-mount-gui-arm64-apple-darwin
./hf-mount-gui-arm64-apple-darwin
```

If macOS blocks the unsigned downloaded app or binary, remove the quarantine attribute or allow it in System Settings:

```bash
xattr -dr com.apple.quarantine ./hf-mount.app
xattr -dr com.apple.quarantine ./hf-mount-gui-arm64-apple-darwin
```

The GUI uses the NFS backend on both Windows and macOS. It does not enable FUSE or overlay mode.

### Windows

Download `hf-mount-windows-x64.exe` from [GitHub Releases](https://github.com/huggingface/hf-mount/releases), enable Client for NFS, then run it from an Administrator PowerShell or Command Prompt. The Windows executable is the foreground NFS backend, so commands do not use `hf-mount start`.

```powershell
$env:HF_TOKEN = "hf_..."

# Mount to a drive letter
.\hf-mount-windows-x64.exe repo openai/gpt-oss-20b Z:

# Directory targets may work on some Windows NFS setups, but a drive letter is recommended.
New-Item -ItemType Directory -Force C:\hf-mounts\gpt-oss | Out-Null
.\hf-mount-windows-x64.exe repo openai/gpt-oss-20b C:\hf-mounts\gpt-oss
```

Stop the mount with Ctrl+C in the foreground process, or unmount it from another Administrator shell:

```powershell
umount -f Z:
```

Windows differences and limitations:
- NFS only; FUSE and the background daemon controller are Unix-only.
- `hf-mount-windows-x64.exe` stays in the foreground and owns the local NFS server until stopped.
- The GUI can run its NFS backend in a detached background worker and can register that worker for login autostart.
- Administrator privileges are required because the Windows NFS client uses the local portmapper on port `111`.
- Use a drive letter such as `Z:` for the most reliable Windows mount target. Empty NTFS directory targets depend on the Windows NFS client and may fail on some systems.
- Drive-letter targets such as `Z:` are mapped by Windows `mount.exe`; hf-mount does not try to create them as directories first.
- Overlay mode (`--overlay`) is not supported on Windows.
- Read-only repo mounts and `--read-only` are enforced by hf-mount; the Windows NFS client does not use a separate read-only mount option.
- POSIX metadata such as uid, gid, chmod modes, and symlinks is best-effort through the Windows NFS client and may not behave exactly like Linux/macOS clients.

### Mount a repo (read-only)

```bash
# Public model (no token needed)
hf-mount start repo openai/gpt-oss-20b /tmp/model

# Private model
hf-mount start --hf-token $HF_TOKEN repo myorg/my-private-model /tmp/model

# Dataset
hf-mount start repo datasets/open-index/hacker-news /tmp/hn

# Specific revision
hf-mount start repo openai-community/gpt2 /tmp/gpt2 --revision v1.0

# Subfolder only
hf-mount start repo openai-community/gpt2/onnx /tmp/onnx
```

### Mount a Bucket (read-write)

[Buckets](https://huggingface.co/docs/hub/storage-buckets) are S3-like object storage on the Hub, designed for large-scale mutable data (training checkpoints, logs, artifacts) without git version control.

```bash
hf-mount start --hf-token $HF_TOKEN bucket myuser/my-bucket /tmp/data

# Read-only
hf-mount start --hf-token $HF_TOKEN --read-only bucket myuser/my-bucket /tmp/data

# Subfolder only
hf-mount start --hf-token $HF_TOKEN bucket myuser/my-bucket/checkpoints /tmp/ckpts
```

### Manage mounts

```bash
hf-mount status                  # list running mounts
hf-mount stop /tmp/data          # stop and unmount
```

Logs are written to `~/.hf-mount/logs/`. PID files are stored in `~/.hf-mount/pids/`.

### FUSE backend

By default, `hf-mount` uses NFS. Pass `--fuse` for tighter kernel integration (page cache invalidation, per-file metadata revalidation). Requires `fuse3` on Linux or [macFUSE](https://osxfuse.github.io/) on macOS.

```bash
hf-mount start --fuse --hf-token $HF_TOKEN bucket myuser/my-bucket /mnt/data
```

### Foreground mode

For scripts, containers, or debugging, use the backend binaries directly (they run in the foreground):

```bash
hf-mount-nfs repo gpt2 /tmp/gpt2
hf-mount-fuse --hf-token $HF_TOKEN bucket myuser/my-bucket /mnt/data
```

### macOS: launch as a daemon with launchd

To have `hf-mount` start automatically on login, create a LaunchAgent:

```bash
label=co.huggingface.hf-mount

mkdir -p ~/Library/LaunchAgents

cat > ~/Library/LaunchAgents/$label.plist <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$label</string>
    <key>ProgramArguments</key>
    <array>
        <string>$HOME/.local/bin/hf-mount-nfs</string>
        <string>repo</string>
        <string>openai/gpt-oss-20b</string>
        <string>/tmp/gpt-oss</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/hf-mount.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/hf-mount.log</string>
</dict>
</plist>
EOF

launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/$label.plist
```

To stop: `launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/$label.plist`

### Unmount

```bash
umount /tmp/data                 # NFS or FUSE (macOS)
fusermount -u /tmp/data          # FUSE (Linux)
hf-mount stop /tmp/data          # daemon mounts
```

### Options

| Flag | Default | Description |
| --- | --- | --- |
| `--hf-token` | `$HF_TOKEN` | HF API token (required for private repos/buckets) |
| `--hub-endpoint` | `https://huggingface.co` | Hub API endpoint |
| `--cache-dir` | system temp dir + `hf-mount-cache` | Local cache directory |
| `--cache-size` | `10000000000` (~10 GB) | Max on-disk chunk cache size in bytes |
| `--read-only` | `false` | Mount read-only (always on for repos) |
| `--advanced-writes` | `false` | Enable staging files + async flush (random writes, seek, overwrite) |
| `--poll-interval-secs` | `30` | Remote change polling interval (0 to disable) |
| `--max-threads` | `16` | Maximum FUSE worker threads (Linux only) |
| `--metadata-ttl-ms` | `10000` | How long file metadata is cached before re-checking (ms) |
| `--metadata-ttl-minimal` | `false` | Re-check on every access (maximum freshness, lower throughput) |
| `--flush-debounce-ms` | `2000` | Advanced writes: flush debounce delay (ms) |
| `--flush-max-batch-window-ms` | `30000` | Advanced writes: max flush batch window (ms) |
| `--no-disk-cache` | `false` | Disable local chunk cache (every read fetches from HF) |
| `--no-filter-os-files` | `false` | Stop filtering OS junk files (.DS_Store, Thumbs.db, etc.) |
| `--uid` / `--gid` | current user | Override UID/GID for mounted files |
| `--fuse-owner-only` | `false` | Restrict mount access to the mounting user only (FUSE only; by default all users can access, which requires `user_allow_other` in /etc/fuse.conf) |
| `--token-file` | | Path to a token file (re-read on each request for credential rotation) |
| `--inode-soft-limit` | `0` | Soft cap on the in-memory inode table (0 disables). See "Bounding inode memory" below. |
| `--lru-sweep-interval-ms` | `5000` | Background LRU sweep interval in milliseconds. Only meaningful when `--inode-soft-limit > 0`. |
| `--overlay` | `false` | Treat the mount point as a writable local layer over the remote source. Local files persist on disk; writes are never pushed to the remote. See "Overlay mode" below. |

### Bounding inode memory

Under workloads that enumerate large trees (a `find`, a documentation scraper, `du -sh`), the in-memory inode table can grow without bound: every path the kernel ever looked up stays resident. With `--inode-soft-limit N` set, two evictors cooperate to keep the table near `N`:

1. **Insert-time evictor** (synchronous): before adding a new entry when `len() >= N + 256`, drop the oldest-touched file/symlink/leaf-directory entries.
   - *Polite mode*: only entries the kernel has already released (`forget`-ed). Safe, no FUSE races.
   - *Force mode*: when above `2 × N` and polite found nothing, drop entries even if the kernel still caches the dentry. A racing kernel op sees ENOENT and re-looks up. **Dirty files, locally-created dirs/symlinks, and inodes with live file handles are never dropped** — the force path preserves all user data.
2. **Background LRU sweep** (every `--lru-sweep-interval-ms`): for inodes the kernel has cached but our table doesn't want, send `FUSE_NOTIFY_INVAL_ENTRY` so the kernel drops its dentry and sends us `forget`. Bounded to 1024 invalidations per sweep with EAGAIN backoff so we don't flood the notify channel.

Tuning: pick `N` below what a full-tree enumeration of your bucket would produce. For `hf-doc-build/doc-dev` with ~20k files, `--inode-soft-limit 10000` keeps sidecar RSS ~250 MiB with a 1 GiB cgroup cap.

### Overlay mode

`--overlay` makes the mount point itself a writable local layer on top of the remote source. Reads return whatever the remote has, plus anything you've put on local disk under the mount point. Writes go only to local disk — the remote is never touched. Local files survive an unmount/remount.

Useful when several machines or processes need to share a read-only remote view but each layer their own files on top — for example, a shared compilation cache where producer machines populate a bucket with compiled artifacts (torch.compile, vLLM, JAX/XLA, AWS Neuron) and every consumer mounts the same bucket with `--overlay`. Cache hits are served from the bucket without recompiling; cache misses compile locally and stay on the local disk, never pushed back to the bucket.

```bash
# Producer (writes compiled artifacts to the bucket — regular bucket mount)
hf-mount start bucket myorg/torch-compile-cache "$TORCHINDUCTOR_CACHE_DIR"

# Consumer (reads from the bucket, compiles locally on miss)
hf-mount start --overlay bucket myorg/torch-compile-cache "$TORCHINDUCTOR_CACHE_DIR"
```

What you can do:
- Read every file from the remote source.
- Read every file already present in the local layer; when a name exists in both, the local copy wins.
- Create new files and directories — they land in the local layer.
- Modify, rename, delete, or chmod any file or directory that lives in the local layer.

What you can't do:
- Modify, rename, delete, or chmod a file that exists only on the remote. These operations fail with a permission error. To diverge from a remote file, copy it under a new name through the mount; the copy is a regular local file you own.
- Shadow an existing remote name with a new local file once the mount is active. If you need a local file at a name that already exists on the remote, drop it in the mount-point directory *before* starting the mount — pre-existing files at the mount point stay visible and take precedence.
- Place symlinks in the local layer and expect them to show up. Symlinks are hidden from the merged view so the mount can't be tricked into reading or writing outside the mount point.

### Logging

```bash
RUST_LOG=hf_mount=debug hf-mount-fuse repo gpt2 /mnt/gpt2
```

## Features

- **FUSE & NFS backends** -- FUSE for standard Linux/macOS, NFS for environments without `/dev/fuse`
- **Lazy loading** -- files are fetched on demand, not eagerly downloaded
- **Subfolder mounting** -- mount only a subdirectory (e.g. `user/model/ckpt/v2`)
- **Simple writes** (default) -- append-only, in-memory, synchronous upload on close
- **Advanced writes** (`--advanced-writes`) -- staging files on disk, random writes + seek, async debounced flush
- **Remote sync** -- background polling detects remote changes and updates the local view
- **POSIX metadata** -- chmod, chown, timestamps, symlinks (in-memory only, lost on unmount)
- **Overlay mode** (`--overlay`) -- mount point doubles as a writable local layer; remote stays read-only

## Consistency model

hf-mount provides **eventual consistency** with remote changes. There is no push notification from the Hub; all freshness relies on client-side polling.

### Reads

Files can be stale for up to `--metadata-ttl-ms` (default 10 s) after a remote update. Two mechanisms detect changes:

1. **Metadata revalidation** (FUSE only) -- when the per-file TTL expires, the next access checks the Hub. If the file changed, cached data is invalidated.
2. **Background polling** (default every 30 s) -- lists the full tree and detects additions, modifications, and deletions.

### Writes

| | Streaming (default) | Advanced (`--advanced-writes`) |
| --- | --- | --- |
| Write pattern | Append-only (sequential) | Random writes, seek, overwrite |
| Storage | In-memory buffer | Local staging file on disk |
| Modify existing files | Overwrite only (O_TRUNC) | Yes (downloads file first) |
| Durability | On close | Async, debounced (2 s / 30 s max) |
| Disk space needed | None | Full file size per open file |

**Streaming mode** buffers writes in memory and uploads on `close()`. A crash before close means data loss.

> **Note:** Streaming mode does not support text editors (vim, nano, emacs). Editors that use
> unlink+create save patterns will be blocked (`EPERM`) to prevent data loss.
> Use `--advanced-writes` for interactive editing.

**Advanced mode** downloads the full file to local disk before allowing edits. After `close()`, dirty files are flushed asynchronously. A crash before flush completes means data loss.

### FUSE vs NFS

| | FUSE | NFS |
| --- | --- | --- |
| Metadata revalidation | Per-file, within TTL | No (NFS uses file handles) |
| Page cache invalidation | Supported | Not supported by NFS protocol |
| Staleness window | ~10 s | Up to poll interval (30 s) |
| Write mode | Streaming by default | Advanced always |

## How it works

hf-mount sits between your application and the Hugging Face Hub. It presents a standard filesystem interface (FUSE or NFS) and translates file operations into Hub API calls and storage fetches.

Reads go through an adaptive prefetch buffer that starts small and grows with sequential access. Writes are uploaded to HF storage and committed via the Hub API. A background poll loop keeps the local view in sync with remote changes.

Built on [xet-core](https://github.com/huggingface/xet-core) for content-addressed storage and efficient file transfers, and [fuser](https://github.com/cberner/fuser) for the FUSE implementation.

## Kubernetes

Use the [hf-csi-driver](https://github.com/huggingface/hf-csi-driver) to mount Buckets and repos as Kubernetes volumes. The CSI driver runs hf-mount inside a DaemonSet and exposes mounts to pods via the Container Storage Interface.

```bash
helm install hf-csi oci://ghcr.io/huggingface/charts/hf-csi-driver
```

See the [hf-csi-driver README](https://github.com/huggingface/hf-csi-driver#readme) for setup and examples.

## Testing

```bash
# Unit tests (no network, no token)
cargo test --lib --features fuse,nfs

# Integration tests (require HF_TOKEN and FUSE)
HF_TOKEN=... cargo test --release --features fuse,nfs --test fuse_ops -- --test-threads=1 --nocapture
HF_TOKEN=... cargo test --release --features fuse,nfs --test nfs_ops -- --test-threads=1 --nocapture

# Repo mount test (public repo, no token needed)
cargo test --release --features nfs --test repo_ops -- --test-threads=1 --nocapture

# Benchmarks
HF_TOKEN=... cargo test --release --features fuse,nfs --test bench -- --nocapture
```

Windows build smoke (run on Windows with Rust installed):

```powershell
cargo test --lib --target x86_64-pc-windows-msvc --no-default-features --features nfs
$env:RUSTFLAGS = "-C target-feature=+crt-static"
cargo build --release --target x86_64-pc-windows-msvc --no-default-features --features nfs --bin hf-mount-nfs
cargo build --release --target x86_64-pc-windows-msvc --no-default-features --features nfs,gui --bin hf-mount-gui
.\target\x86_64-pc-windows-msvc\release\hf-mount-nfs.exe --help
.\target\x86_64-pc-windows-msvc\release\hf-mount-gui.exe --help
.\target\x86_64-pc-windows-msvc\release\hf-mount-gui.exe --check-setup Z:
```

macOS GUI build smoke (run on Apple Silicon macOS with Rust installed):

```bash
cargo build --release --target aarch64-apple-darwin --no-default-features --features nfs,gui,vendored-openssl --bin hf-mount-gui
target/aarch64-apple-darwin/release/hf-mount-gui --help
target/aarch64-apple-darwin/release/hf-mount-gui --check-setup /tmp/hf-mount-gui
bash scripts/package-macos-gui.sh aarch64-apple-darwin arm64 dist
```

## Troubleshooting

> I'm getting `Operation not permitted` on MacOS while opening or listing files in a mounted bucket using VSCode

You need to enable "Full disk access" to your VSCode (System settings > Privacy > Full disk access).

### Windows

**`mount.exe` or `umount.exe` is not recognized**

Enable Client for NFS and open a new Administrator shell. On Windows 10/11, use `Enable-WindowsOptionalFeature`; on Windows Server, use `Install-WindowsFeature`.

**`failed to bind portmapper on 127.0.0.1:111`**

Run the executable as Administrator. If it still fails, another portmapper/NFS service is already using port `111`; stop that service or close the other hf-mount instance.

For the GUI, use **Check setup** or run:

```powershell
.\hf-mount-gui-windows-x64.exe --check-setup Z:
```

**`mount.exe failed ...` or `Network Error - 53` / `Netzwerkfehler - 53`**

Confirm the Client for NFS feature is installed, the shell is elevated, and the mount point is an unused drive letter such as `Z:`. On Windows, hf-mount mounts the local root export as `\\127.0.0.1\\`; network error 53 means Windows could not reach that local NFS export, usually because the process is not elevated, port `111` is unavailable, or localhost NFS/RPC traffic is blocked.

**The mounted drive is not visible in Explorer or non-admin apps**

Windows separates elevated and non-elevated mapped drives by default. Set `EnableLinkedConnections` as shown in [System dependencies (Windows)](#system-dependencies-windows), then reboot.

## License

Apache-2.0
