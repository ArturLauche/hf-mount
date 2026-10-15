import { useCallback } from "react";
import { useToast } from "../components/ui/overlay";
import { errorInfo } from "../lib/api";

/**
 * Run a fire-and-forget promise from an event handler and show a toast if it
 * fails, so no failure is silently dropped as an unhandled rejection.
 */
export function useGuard() {
  const toast = useToast();
  return useCallback(
    (promise: Promise<unknown>, failureTitle: string) => {
      promise.catch((e: unknown) => toast({ tone: "error", title: failureTitle, body: errorInfo(e).message }));
    },
    [toast],
  );
}
