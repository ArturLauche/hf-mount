import { useEffect, useState } from "react";
import { useUnmountReport } from "../hooks/useUnmountReport";
import { api, errorInfo } from "../lib/api";
import { sentence } from "../lib/errors";
import { onAppEvent } from "../lib/native";
import { useApp } from "../lib/state";
import type { BlockedUnmount } from "../lib/types";
import { ConfirmDialog, useToast } from "./ui/overlay";

/**
 * "Unmount all" from the tray and quitting with unmount leave drives with
 * files open for writing mounted; this asks whether to unmount them anyway.
 */
export function BlockedUnmountDialog() {
  const { refresh } = useApp();
  const toast = useToast();
  const reportUnmount = useUnmountReport();
  const [ask, setAsk] = useState<BlockedUnmount | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => onAppEvent<BlockedUnmount>("unmount-blocked", (blocked) => setAsk(blocked)), []);

  const confirm = async () => {
    if (!ask) return;
    setBusy(true);
    try {
      if (ask.quit) {
        await api.quit(true, true);
      } else {
        for (const drive of ask.drives) {
          try {
            reportUnmount(await api.stopMount(drive.target, true));
          } catch (e) {
            toast({ tone: "error", title: `Could not unmount ${drive.target}`, body: sentence(errorInfo(e).message) });
          }
        }
      }
      setAsk(null);
    } catch (e) {
      toast({ tone: "error", title: "Could not quit", body: errorInfo(e).message });
    } finally {
      setBusy(false);
      void refresh();
    }
  };

  const drives = ask?.drives ?? [];
  const one = drives.length === 1;
  return (
    <ConfirmDialog
      open={!!ask}
      onOpenChange={(o) => !o && setAsk(null)}
      title={one ? `Files are open on ${drives[0]!.target}` : `Files are open on ${drives.map((d) => d.target).join(" ")}`}
      body={
        <>
          <ul className="flex flex-col gap-1">
            {drives.map((d) => (
              <li key={d.target}>{sentence(d.message)}</li>
            ))}
          </ul>
          <p className="mt-2">
            {ask?.quit ? "hf-mount did not quit. " : ""}
            Close the apps using {one ? "the drive" : "these drives"} first. If you unmount anyway, those apps lose
            access; finished changes are still uploaded, and anything that cannot be uploaded is kept in the recovered
            files folder.
          </p>
        </>
      }
      confirmLabel={ask?.quit ? "Unmount anyway and quit" : "Unmount anyway"}
      danger
      busy={busy}
      onConfirm={() => void confirm()}
    />
  );
}
