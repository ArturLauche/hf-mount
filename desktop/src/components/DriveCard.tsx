import {
  AlertCircle,
  Copy,
  ExternalLink,
  FolderOpen,
  MoreHorizontal,
  Pin,
  PinOff,
  Play,
  Power,
  RotateCw,
  Save,
  Terminal,
  Trash2,
  UploadCloud,
} from "lucide-react";
import type { DriveRow } from "../lib/drives";
import { errorTitle, sentence } from "../lib/errors";
import { formatBytes, plural, relativeTime } from "../lib/format";
import { displayName, sourceLine, typeLabel } from "../lib/source";
import { Menu, MenuContent, MenuItem, MenuSeparator, MenuTrigger } from "./ui/overlay";
import { Badge, Button, cn, Spinner, Tooltip } from "./ui/primitives";

export type Busy = "mounting" | "unmounting" | undefined;

export interface DriveActions {
  open: (row: DriveRow) => void;
  mount: (row: DriveRow) => void;
  unmount: (row: DriveRow) => void;
  dismiss: (row: DriveRow) => void;
  retryUploads: (row: DriveRow) => void;
  copyPath: (row: DriveRow) => void;
  viewOnHub: (row: DriveRow) => void;
  save: (row: DriveRow) => void;
  forget: (row: DriveRow) => void;
  toggleAutoMount: (row: DriveRow) => void;
}

function Status({ row, busy, letterBusy }: { row: DriveRow; busy: Busy; letterBusy: boolean }) {
  const m = row.mount;
  if (busy === "unmounting" || m?.state === "stopping") {
    const uploads = (m?.stats?.pending_uploads ?? 0) + (m?.stats?.pending_deletes ?? 0);
    return (
      <Badge dot pulse>
        {uploads > 0 ? "Finishing uploads…" : "Unmounting…"}
      </Badge>
    );
  }
  if (busy === "mounting" || m?.state === "starting")
    return (
      <Badge dot pulse>
        Connecting…
      </Badge>
    );
  if (m?.state === "mounted")
    return (
      <Badge tone="ok" dot>
        Mounted
      </Badge>
    );
  if (m?.state === "failed")
    return (
      <Badge tone="danger" dot>
        Failed
      </Badge>
    );
  if (letterBusy) return <Badge tone="warn">Letter in use</Badge>;
  return <Badge tone="outline">Not mounted</Badge>;
}

function Meta({ row }: { row: DriveRow }) {
  const m = row.mount;
  const s = m?.stats;
  const items: { key: string; node: React.ReactNode }[] = [];
  items.push({ key: "mode", node: row.spec.read_only ? "Read-only" : "Read & write" });
  if (m?.state === "mounted" && s) {
    if (s.bytes_read > 0) items.push({ key: "read", node: `${formatBytes(s.bytes_read)} read` });
    if (s.bytes_written > 0) items.push({ key: "written", node: `${formatBytes(s.bytes_written)} written` });
    if (s.open_handles > 0) items.push({ key: "open", node: plural(s.open_handles, "open file") });
    if (s.pending_uploads > 0)
      items.push({
        key: "uploads",
        node: (
          <span className="inline-flex items-center gap-1 text-fg-2">
            <UploadCloud className="size-3.5" aria-hidden />
            {plural(s.pending_uploads, "upload")} pending
          </span>
        ),
      });
    if (s.pending_deletes > 0)
      items.push({
        key: "deletes",
        node: (
          <span className="inline-flex items-center gap-1 text-fg-2">
            <UploadCloud className="size-3.5" aria-hidden />
            {plural(s.pending_deletes, "deletion")} pending
          </span>
        ),
      });
    if (s.failed_uploads > 0)
      items.push({ key: "failed", node: <span className="text-danger">{plural(s.failed_uploads, "upload")} failed</span> });
  } else if (!m && row.profile?.last_mounted_at) {
    items.push({ key: "last", node: `Last mounted ${relativeTime(row.profile.last_mounted_at)}` });
  }
  if (row.profile?.auto_mount) items.push({ key: "auto", node: "Mounts at startup" });
  if (m?.host_kind === "foreground")
    items.push({
      key: "fg",
      node: (
        <Tooltip content="Started with hf-mount start --foreground; it stops when that terminal closes.">
          <span className="inline-flex items-center gap-1">
            <Terminal className="size-3.5" aria-hidden />
            Terminal
          </span>
        </Tooltip>
      ),
    });
  return (
    <p className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[12.5px] text-fg-3">
      {items.map((it, i) => (
        <span key={it.key} className="inline-flex items-center gap-2">
          {i > 0 && <span aria-hidden className="text-border-strong">·</span>}
          {it.node}
        </span>
      ))}
    </p>
  );
}

export function DriveCard({ row, busy, letterBusy, actions }: {
  row: DriveRow;
  busy: Busy;
  letterBusy: boolean;
  actions: DriveActions;
}) {
  const m = row.mount;
  const mounted = m?.state === "mounted";
  const failed = m?.state === "failed";
  const transitioning = !!busy || m?.state === "starting" || m?.state === "stopping";
  const name = row.profile?.name || displayName(row.spec);

  return (
    <article
      className={cn(
        "group rounded-[var(--radius-card)] border bg-surface shadow-[var(--shadow-card)] transition-[border-color] duration-150",
        failed ? "border-danger/30" : "border-border hover:border-border-strong",
      )}
      aria-label={`${row.target} ${name}`}
    >
      <div className="flex items-center gap-4 p-4 pr-3">
        <div
          className={cn(
            "relative grid size-12 shrink-0 place-items-center rounded-[14px] border font-mono text-[15px] font-semibold",
            mounted ? "border-border-strong bg-surface-2 text-fg" : "border-dashed border-border-strong text-fg-3",
          )}
          aria-hidden
        >
          {row.target}
          {mounted && <span className="absolute -right-0.5 -top-0.5 size-2.5 rounded-full border-2 border-surface bg-ok" />}
        </div>

        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 items-center gap-2">
            <h3 className="truncate text-[14.5px] font-semibold tracking-[-0.01em] text-fg">{name}</h3>
            <Status row={row} busy={busy} letterBusy={letterBusy} />
          </div>
          <p className="mt-0.5 flex min-w-0 items-center gap-1.5 text-[12.5px] text-fg-2">
            <span className="shrink-0 font-medium text-fg-2">{typeLabel(row.spec.source_type)}</span>
            <span className="truncate font-mono text-[12px] text-fg-2" title={sourceLine(row.spec)}>
              {sourceLine(row.spec)}
            </span>
          </p>
          <div className="mt-1">
            <Meta row={row} />
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-1.5">
          {mounted && (
            <Button size="sm" icon={<FolderOpen className="size-4" />} onClick={() => actions.open(row)} disabled={transitioning}>
              Open
            </Button>
          )}
          {!m && (
            <Button
              size="sm"
              icon={<Play className="size-3.5" />}
              onClick={() => actions.mount(row)}
              loading={busy === "mounting"}
              disabled={letterBusy}
            >
              Mount
            </Button>
          )}
          {transitioning && !busy && <Spinner />}
          <RowMenu row={row} actions={actions} disabled={transitioning} />
        </div>
      </div>

      {failed && m?.error && (
        <div className="mx-4 mb-4 flex flex-wrap items-start gap-3 rounded-[12px] bg-danger-soft px-3.5 py-3">
          <AlertCircle className="mt-0.5 size-4 shrink-0 text-danger" aria-hidden />
          <div className="min-w-[200px] flex-1 text-[13px]">
            <p className="font-medium text-fg">{errorTitle(m.error)}</p>
            <p className="selectable text-fg-2">{sentence(m.error.message)}</p>
            {m.error.hint && <p className="mt-0.5 text-fg-3">{sentence(m.error.hint)}</p>}
          </div>
          <div className="flex shrink-0 gap-1.5 self-center">
            <Button size="sm" variant="ghost" onClick={() => actions.dismiss(row)}>
              Dismiss
            </Button>
            <Button size="sm" icon={<RotateCw className="size-3.5" />} onClick={() => actions.mount(row)} loading={busy === "mounting"}>
              Try again
            </Button>
          </div>
        </div>
      )}
    </article>
  );
}

function RowMenu({ row, actions, disabled }: { row: DriveRow; actions: DriveActions; disabled: boolean }) {
  const m = row.mount;
  const mounted = m?.state === "mounted";
  return (
    <Menu>
      <Tooltip content="More actions">
        <MenuTrigger asChild>
          <button
            type="button"
            aria-label={`More actions for ${row.target}`}
            className="grid size-8 place-items-center rounded-[10px] text-fg-3 transition-colors hover:bg-surface-2 hover:text-fg data-[state=open]:bg-surface-2 data-[state=open]:text-fg"
          >
            <MoreHorizontal className="size-4" />
          </button>
        </MenuTrigger>
      </Tooltip>
      <MenuContent>
        {mounted && (
          <>
            <MenuItem icon={<FolderOpen />} onSelect={() => actions.open(row)}>
              Open in Explorer
            </MenuItem>
            <MenuItem icon={<Copy />} onSelect={() => actions.copyPath(row)}>
              Copy path
            </MenuItem>
          </>
        )}
        <MenuItem icon={<ExternalLink />} onSelect={() => actions.viewOnHub(row)}>
          View on Hugging Face
        </MenuItem>
        {mounted && (m?.stats?.failed_uploads ?? 0) > 0 && (
          <MenuItem icon={<RotateCw />} onSelect={() => actions.retryUploads(row)}>
            Retry failed uploads
          </MenuItem>
        )}
        <MenuSeparator />
        {row.profile ? (
          <>
            <MenuItem icon={row.profile.auto_mount ? <PinOff /> : <Pin />} onSelect={() => actions.toggleAutoMount(row)}>
              {row.profile.auto_mount ? "Don't mount at startup" : "Mount at startup"}
            </MenuItem>
            <MenuItem icon={<Trash2 />} onSelect={() => actions.forget(row)} disabled={disabled}>
              Remove from saved drives
            </MenuItem>
          </>
        ) : (
          m && (
            <MenuItem icon={<Save />} onSelect={() => actions.save(row)}>
              Save to my drives
            </MenuItem>
          )
        )}
        {m && m.state !== "failed" && (
          <>
            <MenuSeparator />
            <MenuItem icon={<Power />} danger onSelect={() => actions.unmount(row)} disabled={disabled}>
              Unmount {row.target}
            </MenuItem>
          </>
        )}
      </MenuContent>
    </Menu>
  );
}
