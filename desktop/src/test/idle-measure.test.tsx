import { Profiler } from "react";
import { App } from "../App";
import { act, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ToastProvider } from "../components/ui/overlay";
import { useRelativeTime } from "../hooks/useRelativeTime";
import { api } from "../lib/api";
import * as native from "../lib/native";
import { AppStateProvider, useApp } from "../lib/state";
import type { AppInfo, MountInfo, Overview, Settings } from "../lib/types";
import { ActivityPage } from "../pages/Activity";

vi.mock("../lib/api", async (original) => ({ ...await original<typeof import("../lib/api")>(), isTauri: true }));

function snapshot(targets: string[] = []): Overview {
  const mounts: MountInfo[] = targets.map((target) => ({
    id: target,
    spec: { target, id: "org/data", source_type: "model", read_only: true },
    state: "mounted",
    host_pid: 12,
    host_kind: "supervisor",
    requested_at: "now",
    stats: {
      open_handles: 0, open_write_handles: 0, bytes_read: 0, bytes_written: 0,
      failed_operations: 0, pending_uploads: 0, pending_deletes: 0, failed_uploads: 0,
    },
  }));
  return {
    mounts, has_hosts: mounts.length > 0, lost: null, elevated: false, free_letters: ["F:", "G:"],
    driver: { installed: true, bundled_installer: false },
    supervisor: mounts.length ? { pid: 12, kind: "supervisor", version: "1", protocol: 1, started_at: "now", elevated: false } : null,
  };
}

describe("desktop idle and restore", () => {
  let visibility: ((visible: boolean) => void) | undefined;
  let latest: Overview;
  let renders: number;
  let refresh: () => Promise<void>;

  function Probe() {
    const state = useApp();
    renders++;
    refresh = state.refresh;
    return <output data-states={state.overview?.mounts.map((m) => m.state).join(",")}>{state.overview?.mounts.map((m) => m.spec.target).join(",")}{state.overviewError?.message}</output>;
  }

  function start(activity = false) {
    return render(
      <ToastProvider>
        <AppStateProvider><Probe />{activity && <ActivityPage />}</AppStateProvider>
      </ToastProvider>,
    );
  }

  async function nativeVisibility(visible: boolean) {
    expect(visibility).toBeDefined();
    await act(async () => visibility?.(visible));
  }

  beforeEach(() => {
    vi.useFakeTimers();
    latest = snapshot();
    renders = 0;
    vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
    vi.spyOn(native, "onAppEvent").mockImplementation((name, handler) => {
      if (name !== "window-visibility") return () => {};
      visibility = handler as (visible: boolean) => void;
      return () => { visibility = undefined; };
    });
    vi.spyOn(api, "appInfo").mockResolvedValue({} as AppInfo);
    vi.spyOn(api, "settings").mockResolvedValue({ theme: "system" } as Settings);
    vi.spyOn(api, "profiles").mockResolvedValue([]);
    vi.spyOn(api, "accountStatus").mockResolvedValue({ state: "signed_out" });
    vi.spyOn(api, "overview").mockImplementation(async () => structuredClone(latest));
    vi.spyOn(api, "activity").mockResolvedValue([]);
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it.each([
    { name: "visible empty", hide: false, mounted: false, activity: false },
    { name: "hidden empty", hide: true, mounted: false, activity: false },
    { name: "hidden mounted with Activity open", hide: true, mounted: true, activity: true },
    { name: "visible multiple drives", hide: false, mounted: true, activity: false },
    { name: "visible mounted with Activity open", hide: false, mounted: true, activity: true },
    { name: "visible Activity with no hosts", hide: false, mounted: false, activity: true },
  ])("ten minutes $name", async ({ hide, mounted, activity }) => {
    if (mounted) latest = snapshot(["H:", "S:", "X:"]);
    const ui = start(activity);
    await nativeVisibility(true);
    if (hide) await nativeVisibility(false);
    const timers = vi.getTimerCount();
    vi.mocked(api.overview).mockClear();
    vi.mocked(api.activity).mockClear();
    renders = 0;

    await act(async () => { await vi.advanceTimersByTimeAsync(600_000); });

    expect(api.overview).toHaveBeenCalledTimes(hide ? 0 : 76);
    const pollingEvents = !hide && activity && mounted;
    expect(api.activity).toHaveBeenCalledTimes(pollingEvents ? 67 : 0);
    expect(timers).toBe(hide ? 0 : pollingEvents ? 2 : 1);
    expect(renders).toBe(0);
    ui.unmount();
    expect(visibility).toBeUndefined();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("keeps the complete Drives screen idle while three mounts are unchanged", async () => {
    latest = snapshot(["H:", "S:", "X:"]);
    const commits = vi.fn();
    const ui = render(<Profiler id="desktop" onRender={commits}><App /></Profiler>);
    await nativeVisibility(true);
    expect(ui.getAllByRole("article")).toHaveLength(3);
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    commits.mockClear();
    // Flush each poll separately so batching in act cannot conceal commits.
    for (let n = 0; n < 60; n++) {
      await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    }
    expect(commits).not.toHaveBeenCalled();
    await nativeVisibility(false);
    commits.mockClear();
    expect(vi.getTimerCount()).toBe(0);
    await act(async () => { await vi.advanceTimersByTimeAsync(600_000); });
    expect(commits).not.toHaveBeenCalled();
    ui.unmount();
  });

  it("shows a foreground status failure without marking a healthy supervisor unreachable", async () => {
    latest = { ...snapshot(["X:"]), foreground_unavailable: true };
    const ui = render(<App />);
    await nativeVisibility(true);
    expect(ui.getByText("Some foreground drives are unavailable")).toBeInTheDocument();
    expect(ui.getByText("Running · pid 12")).toBeInTheDocument();
    expect(ui.getAllByRole("article")).toHaveLength(1);
    expect(ui.queryByText("Mount service unreachable")).toBeNull();
    ui.unmount();
  });

  it("waits for a live host before polling Activity again after a status failure", async () => {
    latest = { ...snapshot(), status_error: { code: "timeout", message: "The mount host did not answer" } };
    const ui = start(true);
    await nativeVisibility(true);
    expect(ui.getByText("Drive activity is unavailable until a mount host responds.")).toBeInTheDocument();
    vi.mocked(api.activity).mockClear();
    await act(async () => { await vi.advanceTimersByTimeAsync(30_000); });
    expect(api.activity).not.toHaveBeenCalled();

    latest = snapshot(["X:"]);
    vi.mocked(api.activity).mockResolvedValue([{ time: "now", level: "info", message: "Drive mounted" }]);
    await act(async () => { await refresh(); });
    expect(ui.getByText("Drive mounted")).toBeInTheDocument();
    ui.unmount();
  });

  it("defers startup while hidden and discovers an external mount and its events on restore", async () => {
    const ui = start(true);
    await nativeVisibility(false);
    await act(async () => { await vi.advanceTimersByTimeAsync(600_000); });
    expect(api.overview).not.toHaveBeenCalled();
    expect(api.accountStatus).not.toHaveBeenCalled();

    await nativeVisibility(true);
    await nativeVisibility(false);
    latest = snapshot(["X:"]);
    vi.mocked(api.activity).mockResolvedValue([{ time: "now", level: "error", message: "Upload needs attention" }]);
    await nativeVisibility(true);
    expect(ui.getByText("X:")).toBeInTheDocument();
    expect(ui.getByText("Upload needs attention")).toBeInTheDocument();
    expect(api.accountStatus).toHaveBeenCalledTimes(1);
    ui.unmount();
  });

  it("discovers external drive changes during a stable visible session", async () => {
    const ui = start();
    await nativeVisibility(true);
    await act(async () => { await vi.advanceTimersByTimeAsync(30_000); });
    latest = snapshot(["H:"]);
    await act(async () => { await vi.advanceTimersByTimeAsync(8000); });
    expect(ui.getByText("H:")).toHaveAttribute("data-states", "mounted");
    latest.mounts[0]!.state = "failed";
    await act(async () => { await vi.advanceTimersByTimeAsync(8000); });
    expect(ui.getByText("H:")).toHaveAttribute("data-states", "failed");
    latest = snapshot();
    await act(async () => { await vi.advanceTimersByTimeAsync(8000); });
    expect(ui.queryByText("H:")).toBeNull();
    ui.unmount();
  });

  it("keeps relative labels current on the existing visible timer", async () => {
    vi.setSystemTime(new Date("2026-01-01T00:00:00Z"));
    function Stamp() {
      return <span>{useRelativeTime("2026-01-01T00:00:00Z")}</span>;
    }
    const ui = render(<AppStateProvider><Stamp /></AppStateProvider>);
    await nativeVisibility(true);
    expect(ui.getByText("just now")).toBeInTheDocument();
    expect(vi.getTimerCount()).toBe(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(125_000); });
    expect(ui.getByText("2 min ago")).toBeInTheDocument();
    await nativeVisibility(false);
    expect(vi.getTimerCount()).toBe(0);
    await act(async () => { await vi.advanceTimersByTimeAsync(3_600_000); });
    await nativeVisibility(true);
    expect(ui.getByText("1 h ago")).toBeInTheDocument();
    ui.unmount();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("coalesces refreshes around a slow read and discards its obsolete result after hiding", async () => {
    const ui = start();
    let finish!: (value: Overview) => void;
    vi.mocked(api.overview).mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    await nativeVisibility(true);
    await act(async () => { void refresh(); void refresh(); await vi.advanceTimersByTimeAsync(60_000); });
    expect(api.overview).toHaveBeenCalledTimes(1);
    await nativeVisibility(false);
    latest = snapshot(["X:"]);
    await nativeVisibility(true);
    expect(api.overview).toHaveBeenCalledTimes(1);
    await act(async () => finish(snapshot(["H:"])));
    expect(api.overview).toHaveBeenCalledTimes(2);
    expect(ui.getByText("X:")).toBeInTheDocument();
    expect(ui.queryByText("H:")).toBeNull();

    vi.mocked(api.overview).mockRejectedValueOnce({ code: "host_unavailable", message: "Pipe unavailable" });
    await act(async () => { await refresh(); });
    expect(ui.getByText("Pipe unavailable")).toBeInTheDocument();
    expect(ui.queryByText("X:")).toBeNull();
    await act(async () => { await refresh(); });
    expect(ui.getByText("X:")).toBeInTheDocument();
    ui.unmount();
    expect(vi.getTimerCount()).toBe(0);
  });
});
