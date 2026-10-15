import { describe, expect, it } from "vitest";
import { buildRows, letterTaken } from "./drives";
import type { MountInfo, MountSpec, Profile } from "./types";

const spec = (id: string, target: string, extra: Partial<MountSpec> = {}): MountSpec => ({
  source_type: "model",
  id,
  target,
  read_only: true,
  ...extra,
});

const mount = (s: MountSpec, extra: Partial<MountInfo> = {}): MountInfo => ({
  id: `m-${s.target}`,
  spec: s,
  state: "mounted",
  host_pid: 1,
  host_kind: "supervisor",
  requested_at: "2026-01-01T00:00:00Z",
  ...extra,
});

const profile = (s: MountSpec, id = `p-${s.target}`): Profile => ({
  id,
  name: s.id,
  spec: s,
  auto_mount: false,
  use_stored_token: true,
  created_at: "2026-01-01T00:00:00Z",
});

describe("buildRows", () => {
  it("joins a mount with its saved profile", () => {
    const s = spec("org/a", "H:");
    const rows = buildRows([mount(s)], [profile({ ...s, id: "ORG/a" })]);
    expect(rows).toHaveLength(1);
    expect(rows[0]!.mount).toBeDefined();
    expect(rows[0]!.profile).toBeDefined();
  });

  it("lists mounts made elsewhere (CLI) and unmounted profiles", () => {
    const rows = buildRows([mount(spec("org/cli", "Z:"), { host_kind: "foreground" })], [profile(spec("org/saved", "S:"))]);
    expect(rows.map((r) => [r.target, !!r.mount, !!r.profile])).toEqual([
      ["S:", false, true],
      ["Z:", true, false],
    ]);
  });

  it("does not join when the source differs", () => {
    const rows = buildRows([mount(spec("org/a", "H:", { revision: "v2" }))], [profile(spec("org/a", "H:"))]);
    expect(rows).toHaveLength(2);
  });
});

describe("letterTaken", () => {
  it("flags saved drives whose letter is used", () => {
    const rows = buildRows([mount(spec("org/other", "H:"))], [profile(spec("org/a", "H:"))]);
    const saved = rows.find((r) => !r.mount)!;
    expect(letterTaken(saved, rows, ["Z:"])).toBe(true);
    const free = buildRows([], [profile(spec("org/a", "Q:"))]);
    expect(letterTaken(free[0]!, free, ["Q:"])).toBe(false);
    expect(letterTaken(free[0]!, free, ["Z:"])).toBe(true);
  });
});
