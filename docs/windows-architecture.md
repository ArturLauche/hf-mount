# hf-mount for Windows — architecture and licensing decisions

This document records how the Windows edition of hf-mount is built and why.
It is a decision record, not a user guide; see the "Windows" section of the
README for installation and usage.

## 1. Goals and non-goals

* Expose Hugging Face model/dataset/space repositories and storage buckets as
  a **real Windows volume** on a drive letter (`H:`), usable from File
  Explorer, PowerShell and any Win32 application.
* Fetch bytes **lazily** (range reads through the existing Xet/CAS stack and
  its on-disk chunk cache); never clone a repository first.
* Reuse the upstream `VirtualFs` engine (inode table, prefetch, flush manager,
  poll/live-follow) unchanged in behaviour. Windows support is a thin adapter
  plus platform plumbing, not a second filesystem implementation.
* One engine for two front ends: `hf-mount.exe` (CLI) and the desktop app
  (Tauri 2). Both talk to the same per-user mount supervisor.
* Out of scope: WSL, WebDAV, the Windows NFS client, SMB shares, ProjFS/Cloud
  Files hydration (they copy whole files to NTFS) and any "fake drive" that
  is not a filesystem.

## 2. Filesystem integration: WinFsp, bound directly

**Decision.** Use [WinFsp](https://github.com/winfsp/winfsp) ("Windows File
System Proxy") and bind its native user-mode API (`winfsp-x64.dll`) with our
own minimal FFI declarations (`src/windows/winfsp_sys.rs`), loaded at run time.

Alternatives considered:

| Option | Why not |
| --- | --- |
| `winfsp` / `winfsp-sys` Rust crates (winfsp-rs) | Licensed GPL-3.0 without WinFsp's FLOSS linking exception. Linking them would make the combined binaries GPL-3.0, which is incompatible with distributing hf-mount under Apache-2.0. |
| Dokany | Viable LGPL driver, but less actively maintained, weaker security track record, and the brief prefers WinFsp. |
| Windows NFS client (upstream PR #140 approach) | Requires the optional "Client for NFS" Windows feature (not available on Home), a local portmapper and elevated mapping. Explicitly excluded. |
| ProjFS / Cloud Files API | Hydrate whole files into NTFS placeholders; no drive letter; not lazy range reads. |

**Why our own bindings are small and safe enough.** The WinFsp user-mode API
is a C ABI with a handful of structs (`FSP_FSCTL_VOLUME_PARAMS` = 504 bytes,
`FSP_FSCTL_FILE_INFO` = 72, `FSP_FSCTL_DIR_INFO` = 104, a 64-slot interface
vtable) whose sizes WinFsp itself freezes with static asserts. We mirror the
same sizes in Rust with compile-time `assert!`s, and only import ~12
functions. Bitfields are encoded as explicit `u32` flag masks.

**Run-time loading.** The DLL is located through
`HKLM\SOFTWARE\WOW6432Node\WinFsp\InstallDir` (admin-writable only) and loaded
by **absolute path** (no `LoadLibrary("winfsp-x64.dll")` search-order lookup,
which the stock `FspLoad` helper tries first and which is a DLL-planting
vector). Consequences:

* The build needs no WinFsp SDK, `.lib` or headers — CI on a clean runner works.
* A missing driver is a normal, reportable condition (`DriverMissing`, exit
  code 3) instead of a process that fails to start.

### Adapter design (`src/windows/winfsp_fs.rs`)

WinFsp is path based; `VirtualFs` is inode based (FUSE-like). The adapter:

* Resolves `\a\b\c` by walking `VirtualFs::lookup` from the root inode. Lookups
  are served from the in-memory inode table / listing caches and revalidated
  under the existing metadata TTL, exactly like FUSE `lookup`.
* Uses `UmFileContextIsUserContext2`, so each `Open/Create` gets its own
  context object (one Windows handle → one `VirtualFs` file handle), which is
  the same open/release model FUSE uses.
* Opens files **read-only and lazily**; a writable `VirtualFs` handle is only
  acquired on the first write/truncate (`Write`, `SetFileSize`, `Overwrite`).
  Opening an existing bucket file for read/write access therefore does not
  download it until a write actually happens.
* Reads loop until the request is filled or EOF (Windows non-cached I/O, like
  FUSE, must not see short reads mid-file). Offsets are `u64`; files > 4 GiB
  work.
* Directory enumeration returns a name-sorted snapshot per handle and honours
  WinFsp's marker protocol (entries strictly after the marker), plus `.`/`..`
  for non-root directories.
* Maps POSIX errno from `VirtualFs` to NTSTATUS (`src/windows/status.rs`):
  `ENOENT → STATUS_OBJECT_NAME_NOT_FOUND` (or `…PATH_NOT_FOUND` for a missing
  parent), `EROFS → STATUS_MEDIA_WRITE_PROTECTED`, `ENOSPC → STATUS_DISK_FULL`,
  `EACCES/EPERM → STATUS_ACCESS_DENIED`, `EAGAIN → STATUS_DEVICE_BUSY`,
  `ENOTEMPTY → STATUS_DIRECTORY_NOT_EMPTY`, `EEXIST → STATUS_OBJECT_NAME_COLLISION`,
  everything else `STATUS_UNEXPECTED_IO_ERROR`. Never success after a failure.

### Windows semantics

| Concern | Behaviour |
| --- | --- |
| Case sensitivity | The volume is declared **case-sensitive and case-preserving** (`CaseSensitiveSearch=1`). This keeps `README.md` and `readme.md` as distinct remote files instead of letting the kernel merge them into one file node. On read-only mounts a lookup that misses exactly falls back to a case-insensitive match **only when it is unambiguous** (exactly one candidate), so typed paths like `H:\CONFIG.JSON` work. |
| Names Windows cannot represent | Remote names containing `< > : " \| ? *`, control characters, a trailing dot/space, or reserved device names (`CON`, `NUL`, `COM1`…) are **hidden from listings** (and logged once) instead of being shown under a name that would resolve to a different file. Creating such names fails with `STATUS_OBJECT_NAME_INVALID`. |
| Unicode | Names are converted UTF-16 ⇄ UTF-8 losslessly; unpaired surrogates are rejected with `STATUS_OBJECT_NAME_INVALID`. |
| Escaping the root | Every component is validated; `.`/`..`, empty components and separators inside names are rejected before reaching `VirtualFs`. Paths are always relative to the mount root, so nothing can address files outside the mounted repo/bucket prefix. |
| Attributes | Directories: `FILE_ATTRIBUTE_DIRECTORY`; files: `FILE_ATTRIBUTE_ARCHIVE` (`NORMAL` semantics); `FILE_ATTRIBUTE_READONLY` on read-only mounts. Times come from `VirtualFs` (Hub `lastModified`/per-file mtime); creation time = mtime. File IDs are inode numbers. |
| Security | Each volume serves one self-relative security descriptor: owner = mounting user, full access for the user, SYSTEM and Administrators, read/execute for Everyone and AppContainers (so sandboxed apps such as Photos can read). The drive letter itself is created in the user's logon session (`DefineDosDevice` semantics), so other users do not see it. |
| Read-only mounts | `ReadOnlyVolume=1`: the kernel rejects writes with `STATUS_MEDIA_WRITE_PROTECTED` before they reach user mode; the adapter re-checks. Repositories are always read-only (upstream semantics). |
| Caching | `FileInfoTimeout` = metadata TTL (default 1 s on Windows), `FlushAndPurgeOnCleanup=1` so a re-opened file never serves stale cached pages after a remote change; byte caching is done by the Xet chunk cache on disk. |
| Explorer probes | `desktop.ini`, `autorun.inf`, `Thumbs.db`, `folder.jpg`, `$RECYCLE.BIN`, `System Volume Information` … are answered from the parent listing only — they never trigger the HEAD/list-tree "miss probe" that upstream issues for unknown names. Such a file added remotely becomes visible on the next poll. |

### Writes (buckets only)

Windows' cache manager issues lazy, out-of-order, sometimes post-close paging
writes and frequently pre-sizes files (`SetEndOfFile`) before copying. The
upstream "simple" append-only streaming mode cannot honour that, so on Windows
writable mounts **always use upstream's advanced-writes mode** (local staging
file per inode + debounced asynchronous upload), as upstream already does for
NFS.

What this means, stated plainly (also in the README and the app):

* `CloseHandle`/`FlushFileBuffers` return when data is in the local staging
  file, **not** when it is in the bucket. Upload happens in the background
  (debounce 2 s, max batch 30 s by default).
* Pending (not yet uploaded) files are reported by `hf-mount status` and the
  app. `hf-mount stop` waits for uploads (default up to 5 minutes, configurable)
  and reports any file that could not be uploaded; it never claims success for
  data that did not reach the Hub.
* Renames, deletes and overwrites follow upstream semantics (server-side copy
  and delete, not atomic NTFS transactions). `ReplaceFile`/"save via temp file
  then rename over" works through `Rename(ReplaceIfExists)` but is not atomic
  with respect to other writers. There is no multi-writer coordination or byte
  range locking across machines.
* Opening an existing remote file for writing downloads it to staging first
  (copy-on-write), as upstream does.

## 3. Process model and lifecycle

```
 hf-mount.exe (CLI) ─┐                       ┌─► WinFsp volume H:  (VirtualFs #1)
                     ├─ named pipe (JSON) ──►│  hf-mount supervisor (hf-mount.exe supervise)
 desktop app (Tauri)─┘   user-only DACL      └─► WinFsp volume D:  (VirtualFs #2)
```

* **Supervisor.** One background process per user logon session
  (`hf-mount.exe supervise`) hosts every background mount in a shared tokio
  runtime (as upstream's CSI sidecar does). It is started on demand by the CLI
  or the app with `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP |
  CREATE_NO_WINDOW` (and job breakaway when allowed), so closing the terminal
  or the app does not unmount anything. It exits by itself when idle with no
  mounts for a few minutes.
* **Foreground mode** (`hf-mount start … --foreground`) hosts the mount in the
  CLI process and exposes the same control pipe, so `status`/`stop`/the app see
  it too; Ctrl+C or closing the console unmounts cleanly.
* **Control channel.** Named pipe `\\.\pipe\hf-mount-<user SID>-<session id>-<role>`:
  * created with a DACL granting access to the current user SID only,
    `PIPE_REJECT_REMOTE_CLIENTS`, and `FILE_FLAG_FIRST_PIPE_INSTANCE` for the
    supervisor name (no squatting, no duplicate supervisors);
  * clients verify the **server** process's token user SID equals their own
    before sending anything (a token may be sent for private mounts);
  * newline-delimited JSON, versioned (`protocol: 1`).
* **Truthful status.** There are no PID files. State lives only in the live
  host processes; `status` enumerates `\\.\pipe\` for this user/session's
  hosts and asks each one. A host that dies takes its pipe and its WinFsp
  volume with it (the kernel tears the volume down when the process exits), so
  a dead mount can never linger in `status`. A small journal records which
  mounts were active so the next `status` can say "N mounts were lost because
  the supervisor exited unexpectedly" once.
* **Scope.** Same Windows user **and** same logon session (drive letters are
  per logon session). An elevated console has a different logon session than
  Explorer; the CLI warns when run elevated, because drives mounted from an
  elevated process are invisible to the normal Explorer session.
* **Duplicates and races.** The drive letter is checked before mounting
  (`GetLogicalDrives` + `QueryDosDevice`); WinFsp's own mount-point creation is
  the final arbiter, and its collision status is reported as `TargetInUse`.
  The supervisor serialises operations per target letter.

## 4. Credentials and privacy

* Public repositories need no credentials.
* Token sources, in order: `--token-file`, `HF_TOKEN`, `--hf-token`
  (deprecated on Windows; prints a warning because process command lines are
  visible to other processes of the user), the token stored by
  `hf-mount auth login` / the app in **Windows Credential Manager** (generic
  credential `hf-mount:token`, protected by DPAPI per user), then the
  `huggingface_hub` token file (`%HF_HOME%\token`, default
  `%USERPROFILE%\.cache\huggingface\token`).
* Tokens are never placed on a command line. The CLI/app send the token to the
  supervisor over the verified, user-only pipe; the supervisor keeps it in
  memory only. Profiles store a *reference* ("use stored credential"), never
  the secret.
* Logs, IPC error strings, diagnostics exports and the app UI pass through a
  redactor that removes `hf_…` tokens, `Authorization`/`Bearer` values and
  signed-URL query parameters (`X-Amz-*`, `Signature`, `Policy`, `token` …).
* TLS uses the platform verifier (SChannel on Windows through reqwest's
  native-tls); certificate verification is never disabled.

## 5. Driver provisioning

* WinFsp is a signed kernel driver installed machine-wide by its official MSI;
  this needs one administrator approval (UAC) and normally **no reboot**.
* The desktop installer (NSIS, per-user) bundles the **unmodified official
  WinFsp MSI** (permitted by WinFsp's FLOSS exception, see §6) and, after
  asking, launches it; the MSI raises its own UAC prompt. Nothing is installed
  silently or elevated behind the user's back. The app also detects a missing
  driver at start-up and offers the same installer, with a link to
  winfsp.dev as an alternative.
* The CLI zip does not bundle the driver; `hf-mount doctor` explains how to
  install it (`winget install WinFsp.WinFsp` or the official MSI).
* After installation, mounting never needs elevation; the app and CLI run as
  the normal user.

## 6. Licensing

| Component | License | How it is used | Obligations |
| --- | --- | --- | --- |
| hf-mount (this repo) | Apache-2.0 | — | unchanged |
| WinFsp | GPLv3 **with FLOSS exception** | `winfsp-x64.dll` loaded at run time; unmodified MSI redistributed in the desktop installer | Exception permits linking/distributing with OSI-licensed software (Apache-2.0 qualifies) provided: (a) the notice *"WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos"* and a link to https://github.com/winfsp/winfsp appear in the UI and user documentation (About page, README, `hf-mount --version`, `THIRD_PARTY_NOTICES.md`), (b) not combined with proprietary software. |
| winfsp-rs crates | GPL-3.0 | **not used** | — |
| windows-sys | MIT/Apache-2.0 | Win32 API declarations | notice |
| Tauri 2, React, Radix primitives, lucide | MIT/Apache-2.0/ISC | desktop app | notices bundled |
| Geist font | SIL OFL 1.1 | bundled in app | license text bundled |
| Microsoft WebView2 | part of Windows 11 | system component, not redistributed (installer only downloads Microsoft's bootstrapper when absent) | — |

No upstream source file is relicensed. Third-party notices are collected in
`THIRD_PARTY_NOTICES.md` and shipped with both the CLI zip and the installer.

## 7. Testing strategy

* **Pure logic** (path/name validation, NTSTATUS mapping, source parsing,
  redaction, IPC protocol, registry state machine) is unit-tested on every
  platform.
* **Real drive-letter tests** run on GitHub-hosted Windows runners after
  installing the pinned WinFsp MSI (SHA-256 verified): the adapter mounts a
  `VirtualFs` backed by the deterministic in-memory mocks on a free letter and
  checks enumeration (Rust std, `cmd /c dir`, PowerShell), exact bytes,
  random/overlapping reads, concurrent handles, Unicode names, error statuses,
  read-only enforcement, unmount and re-mount.
* **Live smoke test** (CI, public repo, no secrets): `hf-mount start repo …`
  in background mode, `status --json`, read bytes and compare with the Hub's
  `resolve` URL, `stop`, drive letter released.
* Private-token and writable-bucket tests are opt-in (repository secrets) and
  isolated; they are skipped — and reported as skipped — in forks.
