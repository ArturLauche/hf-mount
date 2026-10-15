import { useCallback } from "react";
import { useToast } from "../components/ui/overlay";
import { api } from "../lib/api";
import { plural } from "../lib/format";
import type { UnmountReport } from "../lib/types";
import { useGuard } from "./useGuard";

/** Show the outcome of an unmount, including changes that did not reach the Hub. */
export function useUnmountReport() {
  const toast = useToast();
  const guard = useGuard();
  return useCallback(
    (r: UnmountReport) => {
      const deletes = r.unfinished_deletes ?? 0;
      if (r.unflushed_files === 0 && deletes === 0) {
        toast({ tone: "success", title: `${r.target} unmounted` });
        return;
      }
      const lost = [
        r.unflushed_files > 0 ? plural(r.unflushed_files, "unsaved change") : null,
        deletes > 0 ? plural(deletes, "deletion") + " not done" : null,
      ].filter(Boolean);
      const body = [
        r.recovered_to ? `Copies were kept in ${r.recovered_to}. Upload them again from there.` : null,
        r.kept_at ? `Files that could not be copied were left in ${r.kept_at} (see hf-mount-unsaved.txt there).` : null,
        r.unflushed_files > 0 && !r.recovered_to && !r.kept_at ? "Some changes could not be uploaded." : null,
        deletes > 0 ? `${deletes === 1 ? "A file you deleted is" : "Files you deleted are"} still in the bucket.` : null,
      ]
        .filter(Boolean)
        .join(" ");
      toast({
        tone: "warning",
        title: `${r.target} unmounted with ${lost.join(" and ")}`,
        body,
        action: r.recovered_to
          ? { label: "Open folder", onClick: () => guard(api.openPath("recovered"), "Could not open the folder") }
          : undefined,
      });
    },
    [guard, toast],
  );
}
