import { act, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "./api";
import { AppStateProvider, useApp } from "./state";
import type { Overview, Settings } from "./types";

const overview = (n: number): Overview => ({
  driver: { installed: true, bundled_installer: false },
  supervisor: null,
  mounts: [],
  lost: null,
  elevated: false,
  free_letters: [`${String.fromCharCode(67 + n)}:`],
});

afterEach(() => vi.restoreAllMocks());

describe("refresh", () => {
  it("re-runs a refresh requested while one is in flight", async () => {
    let calls = 0;
    const resolvers: ((o: Overview) => void)[] = [];
    vi.spyOn(api, "overview").mockImplementation(
      () =>
        new Promise<Overview>((resolve) => {
          calls++;
          resolvers.push(resolve);
        }),
    );
    let ctx: ReturnType<typeof useApp> | null = null;
    function Probe() {
      ctx = useApp();
      return null;
    }
    render(
      <AppStateProvider>
        <Probe />
      </AppStateProvider>,
    );
    await waitFor(() => expect(calls).toBe(1)); // initial load in flight
    let done = false;
    act(() => {
      void ctx!.refresh().then(() => {
        done = true;
      });
    });
    await act(async () => resolvers[0]!(overview(1)));
    // The request made during the first one runs afterwards, not dropped.
    await waitFor(() => expect(calls).toBe(2));
    expect(done).toBe(false);
    await act(async () => resolvers[1]!(overview(2)));
    await waitFor(() => expect(done).toBe(true));
    expect(ctx!.overview?.free_letters).toEqual(["E:"]);
  });
});

describe("updateSettings", () => {
  it("keeps both changes when a second control changes before the first save answers", async () => {
    const initial: Settings = {
      theme: "system",
      cache_dir: null,
      cache_size_bytes: 10_000_000_000,
      upload_wait_secs: 300,
      metadata_ttl_ms: 10_000,
      launch_at_login: false,
      reconnect_on_start: true,
      close_to_tray: true,
      unmount_on_quit: false,
      hub_endpoint: null,
    };
    let disk = initial;
    const pending: (() => void)[] = [];
    vi.spyOn(api, "settings").mockResolvedValue(initial);
    const save = vi.spyOn(api, "saveSettings").mockImplementation(
      (s: Settings) =>
        new Promise<Settings>((resolve) => {
          pending.push(() => {
            disk = s;
            resolve(s);
          });
        }),
    );
    let ctx: ReturnType<typeof useApp> | null = null;
    function Probe() {
      ctx = useApp();
      return null;
    }
    render(
      <AppStateProvider>
        <Probe />
      </AppStateProvider>,
    );
    await waitFor(() => expect(ctx!.settings).not.toBeNull());

    let first!: Promise<void>;
    let second!: Promise<void>;
    act(() => {
      first = ctx!.updateSettings({ launch_at_login: true });
      second = ctx!.updateSettings({ close_to_tray: false });
    });
    await waitFor(() => expect(pending).toHaveLength(1));
    await act(async () => pending[0]!());
    await act(async () => first);
    await waitFor(() => expect(pending).toHaveLength(2));
    await act(async () => pending[1]!());
    await act(async () => second);

    expect(save).toHaveBeenCalledTimes(2);
    expect(disk.launch_at_login).toBe(true);
    expect(disk.close_to_tray).toBe(false);
    expect(ctx!.settings).toMatchObject({ launch_at_login: true, close_to_tray: false });
  });

  it("a failed save changes nothing and later saves still apply", async () => {
    const initial = { theme: "system", launch_at_login: false, close_to_tray: true } as Settings;
    vi.spyOn(api, "settings").mockResolvedValue(initial);
    vi.spyOn(api, "saveSettings")
      .mockRejectedValueOnce(new Error("disk full"))
      .mockImplementation(async (s: Settings) => s);
    let ctx: ReturnType<typeof useApp> | null = null;
    function Probe() {
      ctx = useApp();
      return null;
    }
    render(
      <AppStateProvider>
        <Probe />
      </AppStateProvider>,
    );
    await waitFor(() => expect(ctx!.settings).not.toBeNull());
    await act(async () => {
      await expect(ctx!.updateSettings({ launch_at_login: true })).rejects.toThrow("disk full");
      await ctx!.updateSettings({ close_to_tray: false });
    });
    expect(ctx!.settings).toMatchObject({ launch_at_login: false, close_to_tray: false });
  });
});
