import { act, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "./api";
import { AppStateProvider, useApp } from "./state";
import type { Overview } from "./types";

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
