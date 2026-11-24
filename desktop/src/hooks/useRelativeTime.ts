import { useSyncExternalStore } from "react";
import { relativeTime } from "../lib/format";

const listeners = new Set<() => void>();
let sampledAt = new Date();

function subscribe(listener: () => void) {
  if (!listeners.size) sampledAt = new Date();
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

/** Share the existing visible overview tick across the displayed timestamps. */
export function updateRelativeTimes() {
  if (!listeners.size) return;
  sampledAt = new Date();
  for (const listener of listeners) listener();
}

export function useRelativeTime(iso: string) {
  return useSyncExternalStore(subscribe, () => relativeTime(iso, sampledAt));
}
