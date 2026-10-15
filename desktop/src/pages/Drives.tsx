import { HardDrive, Plus, Sparkles } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import type { Page } from "../App";
import { Banner, PageHeader } from "../components/common";
import { DriveCard, type Busy, type DriveActions } from "../components/DriveCard";
import { NewDriveDialog, type MountRequest, type NewDriveInitial } from "../components/NewDriveDialog";
import { ConfirmDialog, useToast } from "../components/ui/overlay";
import { Button, Card, Kbd, Spinner } from "../components/ui/primitives";
import { api, errorInfo } from "../lib/api";
import { buildRows, letterTaken, newProfileId, type DriveRow } from "../lib/drives";
import { errorTitle, sentence } from "../lib/errors";
import { copyText, openUrl, WINFSP_URL } from "../lib/native";
import { displayName, hubUrl } from "../lib/source";
import { useApp } from "../lib/state";
import type { MountInfo, Profile, UnmountReport } from "../lib/types";
import { plural } from "../lib/format";
import { useGuard } from "../hooks/useGuard";

const SUGGESTIONS: NewDriveInitial[] = [
  { source_type: "model", id: "openai-community/gpt2" },
  { source_type: "dataset", id: "HuggingFaceFW/fineweb-edu" },
];

export function DrivesPage({ newDriveRequest, onNavigate }: { newDriveRequest: number; onNavigate: (p: Page) => void }) {
  const { overview, overviewError, profiles, setProfiles, refresh, account } = useApp();
  const toast = useToast();
  const guard = useGuard();
  const [busy, setBusy] = useState<Record<string, Busy>>({});
  const [dialog, setDialog] = useState<{ open: boolean; initial?: NewDriveInitial }>({ open: false });
  const [forceAsk, setForceAsk] = useState<{ row: DriveRow; message: string } | null>(null);
  const [forgetAsk, setForgetAsk] = useState<DriveRow | null>(null);
  const [installing, setInstalling] = useState(false);

  useEffect(() => {
    if (newDriveRequest > 0) setDialog({ open: true });
  }, [newDriveRequest]);

  const rows = useMemo(() => buildRows(overview?.mounts ?? [], profiles), [overview, profiles]);
  const free = overview?.free_letters ?? [];
  const driverInstalled = overview?.driver.installed ?? true;

  const setRowBusy = (target: string, b: Busy) => setBusy((all) => ({ ...all, [target]: b }));

  const persistProfile = useCallback(
    async (p: Profile) => {
      try {
        setProfiles(await api.saveProfile(p));
      } catch (e) {
        toast({ tone: "error", title: "Could not save the drive", body: errorInfo(e).message });
      }
    },
    [setProfiles, toast],
  );

  const installDriver = useCallback(async () => {
    if (!overview?.driver.bundled_installer) {
      await openUrl(WINFSP_URL);
      return;
    }
    setInstalling(true);
    try {
      await api.installDriver();
      toast({ tone: "info", title: "WinFsp installer finished", body: "hf-mount checks the driver again automatically." });
      await refresh();
    } catch (e) {
      const info = errorInfo(e);
      toast({ tone: "error", title: errorTitle(info), body: info.message });
    } finally {
      setInstalling(false);
    }
  }, [overview, refresh, toast]);

  const reportUnmount = useCallback(
    (r: UnmountReport) => {
      if (r.unflushed_files > 0) {
        toast({
          tone: "warning",
          title: `${r.target} unmounted with ${plural(r.unflushed_files, "unsaved change")}`,
          body: r.recovered_to
            ? `Copies were kept in ${r.recovered_to}. Upload them again from there.`
            : "Some changes could not be uploaded.",
          action: r.recovered_to ? { label: "Open folder", onClick: () => guard(api.openPath("recovered"), "Could not open the folder") } : undefined,
        });
      } else {
        toast({ tone: "success", title: `${r.target} unmounted` });
      }
    },
    [guard, toast],
  );

  const doMount = useCallback(
    async (req: MountRequest, existing?: Profile): Promise<MountInfo> => {
      const target = req.spec.target;
      setRowBusy(target, "mounting");
      try {
        const spec = await api.validateSpec(req.spec);
        const mount = await api.startMount(spec, existing?.use_stored_token ?? true);
        const stamp = new Date().toISOString();
        if (existing) {
          await persistProfile({ ...existing, last_mounted_at: stamp });
        } else if (req.save) {
          await persistProfile({
            id: newProfileId(),
            name: displayName(spec),
            spec,
            auto_mount: req.autoMount,
            use_stored_token: true,
            created_at: stamp,
            last_mounted_at: stamp,
          });
        }
        toast({
          tone: "success",
          title: `Mounted at ${mount.spec.target}`,
          body: mount.resolved_source ?? undefined,
          action: { label: "Open in Explorer", onClick: () => guard(api.openDrive(mount.spec.target), "Could not open Explorer") },
        });
        return mount;
      } finally {
        setRowBusy(target, undefined);
        void refresh();
      }
    },
    [guard, persistProfile, refresh, toast],
  );

  const unmount = useCallback(
    async (row: DriveRow, force: boolean) => {
      setRowBusy(row.target, "unmounting");
      try {
        reportUnmount(await api.stopMount(row.target, force));
        setForceAsk(null);
      } catch (e) {
        const info = errorInfo(e);
        if (info.code === "busy_with_open_files" && !force) {
          setForceAsk({ row, message: sentence(info.message) });
        } else {
          setForceAsk(null);
          toast({ tone: "error", title: `Could not unmount ${row.target}`, body: sentence(info.message) });
        }
      } finally {
        setRowBusy(row.target, undefined);
        void refresh();
      }
    },
    [refresh, reportUnmount, toast],
  );

  const actions: DriveActions = {
    open: (row) => guard(api.openDrive(row.target), "Could not open Explorer"),
    mount: (row) => {
      const go = async () => {
        if (row.mount?.state === "failed") await api.dismissMount(row.target).catch(() => {});
        try {
          await doMount({ spec: row.spec, save: false, autoMount: false }, row.profile);
        } catch (e) {
          const info = errorInfo(e);
          toast({ tone: "error", title: `${row.target}: ${errorTitle(info)}`, body: sentence(info.message) });
        }
      };
      void go();
    },
    unmount: (row) => void unmount(row, false),
    dismiss: (row) => guard(api.dismissMount(row.target).finally(() => void refresh()), "Could not dismiss"),
    retryUploads: (row) =>
      void api
        .retryUploads(row.target)
        .then(() => toast({ tone: "info", title: `Retrying uploads on ${row.target}` }))
        .catch((e) => toast({ tone: "error", title: "Could not retry uploads", body: errorInfo(e).message })),
    copyPath: (row) =>
      guard(
        copyText(`${row.target}\\`).then(() => toast({ tone: "success", title: `Copied ${row.target}\\` })),
        "Could not copy",
      ),
    viewOnHub: (row) => guard(openUrl(hubUrl(row.spec)), "Could not open the browser"),
    save: (row) =>
      void persistProfile({
        id: newProfileId(),
        name: displayName(row.spec),
        spec: row.spec,
        auto_mount: false,
        use_stored_token: true,
        created_at: new Date().toISOString(),
        last_mounted_at: row.mount?.mounted_at ?? null,
      }).then(() => toast({ tone: "success", title: `Saved ${row.target}` })),
    forget: (row) => setForgetAsk(row),
    toggleAutoMount: (row) => row.profile && void persistProfile({ ...row.profile, auto_mount: !row.profile.auto_mount }),
  };

  const lost = overview?.lost;
  const mountedCount = rows.filter((r) => r.mount?.state === "mounted").length;
  const savedOnly = rows.filter((r) => !r.mount).length;

  return (
    <>
      <PageHeader
        title="Drives"
        description={
          rows.length > 0
            ? [mountedCount > 0 && `${mountedCount} mounted`, savedOnly > 0 && `${savedOnly} saved`].filter(Boolean).join(" · ")
            : "Hugging Face models, datasets and buckets as Windows drives."
        }
        actions={
          <Button variant="primary" icon={<Plus className="size-4" />} onClick={() => setDialog({ open: true })} title="New drive (Ctrl+N)">
            New drive
          </Button>
        }
      />

      <div className="flex flex-col gap-3">
        {overviewError && (
          <Banner
            tone="danger"
            title={errorTitle(overviewError)}
            actions={
              <Button size="sm" onClick={() => void refresh()}>
                Retry
              </Button>
            }
          >
            {sentence(overviewError.message)}
          </Banner>
        )}

        {overview && !overview.driver.installed && (
          <Banner
            tone="warn"
            title="Install WinFsp to create drives"
            actions={
              <Button size="sm" variant="primary" onClick={() => void installDriver()} loading={installing}>
                {overview.driver.bundled_installer ? "Install WinFsp" : "Get WinFsp"}
              </Button>
            }
          >
            hf-mount uses WinFsp, a free file system driver for Windows, to show repositories as drives. Installing it
            asks for administrator approval once.
            {overview.driver.error && (
              <span className="mt-1 block text-fg-3">{sentence(overview.driver.error)}</span>
            )}
          </Banner>
        )}

        {overview?.elevated && (
          <Banner tone="warn" title="Running as administrator">
            Drives created now are only visible to apps that also run as administrator, not to Explorer. Start hf-mount
            normally to use the drives in Explorer.
          </Banner>
        )}

        {lost && (
          <Banner
            tone="warn"
            title="The mount service stopped unexpectedly"
            actions={
              <>
                <Button size="sm" variant="ghost" onClick={() => guard(api.openPath("recovered"), "Could not open the folder")}>
                  Recovered files
                </Button>
                <Button size="sm" onClick={() => guard(api.dismissLost().finally(() => void refresh()), "Could not dismiss")}>
                  Dismiss
                </Button>
              </>
            }
          >
            {plural(lost.mounts.length, "drive")} disconnected: {lost.mounts.map((m) => m.target).join(", ")}. Mount
            them again below. Changes that had not finished uploading are kept in the recovered files folder.
          </Banner>
        )}

        {!overview && !overviewError && (
          <div className="flex items-center gap-2 py-10 text-[13px] text-fg-3" role="status">
            <Spinner /> Loading drives…
          </div>
        )}

        {overview && rows.length === 0 && (
          <EmptyState onNew={(initial) => setDialog({ open: true, initial })} />
        )}

        {rows.length > 0 && (
          <div className="flex flex-col gap-2.5" role="list" aria-label="Drives">
            {rows.map((row) => (
              <div role="listitem" key={row.key}>
                <DriveCard row={row} busy={busy[row.target]} letterBusy={letterTaken(row, rows, free)} actions={actions} />
              </div>
            ))}
          </div>
        )}
      </div>

      <NewDriveDialog
        open={dialog.open}
        onOpenChange={(open) => setDialog((d) => ({ ...d, open }))}
        initial={dialog.initial}
        freeLetters={free}
        profiles={profiles}
        account={account}
        driverInstalled={driverInstalled}
        onSubmit={(req) => doMount(req)}
        onSignIn={() => {
          setDialog({ open: false });
          onNavigate("account");
        }}
        onInstallDriver={() => void installDriver()}
      />

      <ConfirmDialog
        open={!!forceAsk}
        onOpenChange={(o) => !o && setForceAsk(null)}
        title={`Files are open on ${forceAsk?.row.target ?? ""}`}
        body={
          <>
            <p>{forceAsk?.message}</p>
            <p className="mt-2">
              Close the apps using the drive first. If you unmount anyway, those apps lose access; finished changes are
              still uploaded, and anything that cannot be uploaded is kept in the recovered files folder.
            </p>
          </>
        }
        confirmLabel="Unmount anyway"
        danger
        busy={!!forceAsk && busy[forceAsk.row.target] === "unmounting"}
        onConfirm={() => forceAsk && void unmount(forceAsk.row, true)}
      />

      <ConfirmDialog
        open={!!forgetAsk}
        onOpenChange={(o) => !o && setForgetAsk(null)}
        title={`Remove ${forgetAsk?.profile?.name ?? ""} from saved drives?`}
        body={
          forgetAsk?.mount
            ? "The drive stays mounted for now; it will not appear in the list after it is unmounted. Cached data and files in the repository are not affected."
            : "It will no longer be listed or mounted at startup. Cached data and files in the repository are not affected."
        }
        confirmLabel="Remove"
        onConfirm={() => {
          const p = forgetAsk?.profile;
          setForgetAsk(null);
          if (p)
            void api
              .deleteProfile(p.id)
              .then(setProfiles)
              .catch((e) => toast({ tone: "error", title: "Could not remove", body: errorInfo(e).message }));
        }}
      />
    </>
  );
}

function EmptyState({ onNew }: { onNew: (initial?: NewDriveInitial) => void }) {
  return (
    <Card className="flex flex-col items-center px-8 py-14 text-center">
      <div className="grid size-14 place-items-center rounded-[18px] border border-border bg-surface-2">
        <HardDrive className="size-6 text-fg-2" aria-hidden />
      </div>
      <h2 className="mt-5 text-[17px] font-semibold tracking-[-0.01em] text-fg">No drives yet</h2>
      <p className="mt-1.5 max-w-[420px] text-[13.5px] leading-relaxed text-fg-2">
        Mount a model, dataset or bucket from Hugging Face as a drive letter. Apps read files directly; only the parts
        they read are downloaded.
      </p>
      <div className="mt-6 flex items-center gap-3">
        <Button variant="primary" icon={<Plus className="size-4" />} onClick={() => onNew()}>
          New drive
        </Button>
        <span className="text-[12px] text-fg-3">
          <Kbd>Ctrl</Kbd> <Kbd>N</Kbd>
        </span>
      </div>
      <div className="mt-8 flex flex-wrap items-center justify-center gap-2 text-[12.5px] text-fg-3">
        <Sparkles className="size-3.5" aria-hidden />
        Try a public one:
        {SUGGESTIONS.map((s) => (
          <button
            key={s.id}
            type="button"
            onClick={() => onNew(s)}
            className="rounded-[8px] border border-border px-2 py-0.5 font-mono text-[12px] text-fg-2 transition-colors hover:border-border-strong hover:text-fg "
          >
            {s.id}
          </button>
        ))}
      </div>
    </Card>
  );
}
