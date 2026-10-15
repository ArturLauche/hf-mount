import { AlertTriangle, CheckCircle2, ClipboardCopy, FolderOpen, Info, RefreshCw, XCircle } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { PageHeader } from "../components/common";
import { useToast } from "../components/ui/overlay";
import { Button, Card, cn, Segmented, Spinner } from "../components/ui/primitives";
import { api, errorInfo } from "../lib/api";
import { clockTime } from "../lib/format";
import { copyText } from "../lib/native";
import type { ActivityEvent } from "../lib/types";
import { useGuard } from "../hooks/useGuard";

type Tab = "events" | "log";
type Filter = "all" | "problems";

const REFRESH_MS = 3000;

function sameJson(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

export function ActivityPage() {
  const toast = useToast();
  const guard = useGuard();
  const [tab, setTab] = useState<Tab>("events");
  const [filter, setFilter] = useState<Filter>("all");
  const [events, setEvents] = useState<ActivityEvent[] | null>(null);
  const [log, setLog] = useState<string[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [copying, setCopying] = useState(false);
  const logBox = useRef<HTMLDivElement>(null);
  // Follow new log lines only while the reader is at the bottom.
  const followLog = useRef(true);

  const load = useCallback(async () => {
    try {
      if (tab === "events") {
        const next = await api.activity(300);
        setEvents((prev) => (sameJson(prev, next) ? prev : next));
      } else {
        const next = await api.logTail(400);
        setLog((prev) => (sameJson(prev, next) ? prev : next));
      }
      setLoadError(null);
    } catch (e) {
      setLoadError(errorInfo(e).message);
    }
  }, [tab]);

  useEffect(() => {
    void load();
    const t = setInterval(() => {
      if (document.visibilityState === "visible") void load();
    }, REFRESH_MS);
    return () => clearInterval(t);
  }, [load]);

  useEffect(() => {
    const box = logBox.current;
    if (tab === "log" && box && followLog.current) box.scrollTop = box.scrollHeight;
  }, [log, tab]);

  const copyDiagnostics = async () => {
    setCopying(true);
    try {
      await copyText(await api.diagnostics());
      toast({ tone: "success", title: "Diagnostics copied", body: "Tokens and signed links are removed before copying." });
    } catch (e) {
      toast({ tone: "error", title: "Could not copy diagnostics", body: errorInfo(e).message });
    } finally {
      setCopying(false);
    }
  };

  const shown = (events ?? []).filter((e) => filter === "all" || e.level !== "info").slice().reverse();

  return (
    <>
      <PageHeader
        title="Activity"
        description="What hf-mount did recently, for this Windows session."
        actions={
          <>
            <Button icon={<FolderOpen className="size-4" />} onClick={() => guard(api.openPath("logs"), "Could not open the logs folder")}>
              Logs folder
            </Button>
            <Button icon={<ClipboardCopy className="size-4" />} onClick={() => void copyDiagnostics()} loading={copying}>
              Copy diagnostics
            </Button>
          </>
        }
      />

      <div className="mb-3 flex flex-wrap items-center justify-between gap-3">
        <Segmented
          label="View"
          value={tab}
          onChange={setTab}
          options={[
            { value: "events", label: "Events" },
            { value: "log", label: "Service log" },
          ]}
        />
        {tab === "events" && (
          <Segmented
            size="sm"
            label="Filter"
            value={filter}
            onChange={setFilter}
            options={[
              { value: "all", label: "All" },
              { value: "problems", label: "Problems" },
            ]}
          />
        )}
        {tab === "log" && (
          <Button size="sm" variant="ghost" icon={<RefreshCw className="size-3.5" />} onClick={() => void load()}>
            Refresh
          </Button>
        )}
      </div>

      {loadError && <p className="mb-3 text-[13px] text-danger">{loadError}</p>}

      {tab === "events" && (
        <Card className="overflow-hidden">
          {events === null ? (
            <div className="flex items-center gap-2 p-6 text-[13px] text-fg-3" role="status">
              <Spinner /> Loading…
            </div>
          ) : shown.length === 0 ? (
            <div className="flex flex-col items-center px-6 py-12 text-center">
              <CheckCircle2 className="size-6 text-fg-3" aria-hidden />
              <p className="mt-3 text-[14px] font-medium text-fg">{filter === "problems" ? "No problems" : "Nothing yet"}</p>
              <p className="mt-1 text-[13px] text-fg-3">
                {filter === "problems" ? "Warnings and errors will show up here." : "Mounts, unmounts and errors will show up here."}
              </p>
            </div>
          ) : (
            <ol className="divide-y divide-border" aria-label="Events">
              {shown.map((e, i) => (
                <EventRow key={`${e.time}-${i}`} event={e} />
              ))}
            </ol>
          )}
        </Card>
      )}

      {tab === "log" && (
        <Card className="overflow-hidden">
          <div
            ref={logBox}
            onScroll={(e) => {
              const el = e.currentTarget;
              followLog.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
            }}
            className="selectable max-h-[calc(100vh-260px)] min-h-[240px] overflow-auto bg-surface p-4 font-mono text-[12px] leading-[1.65] text-fg-2"
          >
            {log === null ? (
              <span className="text-fg-3">Loading…</span>
            ) : log.length === 0 ? (
              <span className="text-fg-3">The log is empty. It is written by the mount service while it runs.</span>
            ) : (
              log.map((line, i) => (
                <div
                  key={i}
                  className={cn(
                    "whitespace-pre-wrap break-all",
                    / ERROR /.test(line) && "text-danger",
                    / WARN /.test(line) && "text-warn",
                  )}
                >
                  {line}
                </div>
              ))
            )}
          </div>
          <p className="border-t border-border px-4 py-2.5 text-[12px] text-fg-3">
            Tokens, authorization headers and signed links are removed before anything is written to the log.
          </p>
        </Card>
      )}
    </>
  );
}

const levelIcon = {
  info: <Info className="size-4 text-fg-3" aria-label="Info" />,
  warning: <AlertTriangle className="size-4 text-warn" aria-label="Warning" />,
  error: <XCircle className="size-4 text-danger" aria-label="Error" />,
};

function EventRow({ event }: { event: ActivityEvent }) {
  return (
    <li className="flex items-start gap-3 px-4 py-3">
      <span className="mt-0.5 shrink-0">{levelIcon[event.level]}</span>
      <div className="min-w-0 flex-1">
        <p className="selectable break-words text-[13px] text-fg">
          {event.message.charAt(0).toUpperCase() + event.message.slice(1)}
        </p>
      </div>
      {event.target && (
        <span className="shrink-0 rounded-[7px] border border-border px-1.5 font-mono text-[11.5px] leading-5 text-fg-2">
          {event.target}
        </span>
      )}
      <time dateTime={event.time} className="w-[72px] shrink-0 text-right text-[12px] tabular-nums text-fg-3">
        {clockTime(event.time)}
      </time>
    </li>
  );
}
