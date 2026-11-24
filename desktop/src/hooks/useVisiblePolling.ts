import { useCallback, useEffect, useRef } from "react";

/** Poll after the previous read completes. A restore or user action racing a
 * read needs a snapshot started after that action, even if the old read succeeds. */
export function useVisiblePolling(visible: boolean, read: (signal: AbortSignal) => Promise<boolean>, minMs: number, maxMs: number) {
  const flight = useRef<Promise<void> | null>(null);
  const queued = useRef<(() => Promise<boolean>) | null>(null);
  const delay = useRef(minMs);
  const session = useRef<AbortController | null>(null);

  const run = useCallback((fresh: boolean) => {
    const signal = session.current?.signal;
    if (!signal) return Promise.resolve();
    const load = () => read(signal);
    if (flight.current) {
      if (fresh) queued.current = load;
      return flight.current;
    }
    async function drain() {
      let next: (() => Promise<boolean>) | null = load;
      try {
        do {
          queued.current = null;
          const changed = await next();
          delay.current = changed ? minMs : Math.min(maxMs, delay.current * 2);
          next = queued.current;
        } while (next);
      } finally {
        flight.current = null;
      }
    }
    flight.current = drain();
    return flight.current;
  }, [read, minMs, maxMs]);

  useEffect(() => {
    if (!visible) return;
    const current = new AbortController();
    session.current = current;
    let timer: ReturnType<typeof setTimeout>;
    async function tick(fresh: boolean) {
      if (current.signal.aborted) return;
      await run(fresh);
      if (!current.signal.aborted) timer = setTimeout(() => void tick(false), delay.current);
    }
    void Promise.resolve().then(() => tick(true));
    return () => {
      current.abort();
      session.current = null;
      queued.current = null;
      clearTimeout(timer);
    };
  }, [visible, run]);

  return useCallback(() => run(true), [run]);
}
