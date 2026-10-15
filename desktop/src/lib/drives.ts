import type { MountInfo, MountSpec, Profile } from "./types";

/** One row of the Drives page: a live mount, a saved profile, or both. */
export interface DriveRow {
  key: string;
  target: string;
  spec: MountSpec;
  mount?: MountInfo;
  profile?: Profile;
}

function sameSource(a: MountSpec, b: MountSpec): boolean {
  return (
    a.source_type === b.source_type &&
    a.id.toLowerCase() === b.id.toLowerCase() &&
    (a.subfolder ?? "") === (b.subfolder ?? "") &&
    (a.source_type === "bucket" || (a.revision ?? "main") === (b.revision ?? "main"))
  );
}

/**
 * Merge live mounts (from every host: app, CLI, foreground) with saved
 * profiles. A profile and a mount are the same drive when they use the same
 * letter and source. Mounts created elsewhere (e.g. by the CLI) appear even
 * without a profile; profiles that are not mounted appear as "not mounted".
 */
export function buildRows(mounts: MountInfo[], profiles: Profile[]): DriveRow[] {
  const rows: DriveRow[] = [];
  const used = new Set<string>();
  for (const mount of mounts) {
    const profile = profiles.find((p) => !used.has(p.id) && p.spec.target === mount.spec.target && sameSource(p.spec, mount.spec));
    if (profile) used.add(profile.id);
    rows.push({ key: `m:${mount.id}`, target: mount.spec.target, spec: mount.spec, mount, profile });
  }
  for (const profile of profiles) {
    if (used.has(profile.id)) continue;
    rows.push({ key: `p:${profile.id}`, target: profile.spec.target, spec: profile.spec, profile });
  }
  return rows.sort((a, b) => a.target.localeCompare(b.target) || a.key.localeCompare(b.key));
}

/** Whether a not-mounted profile's letter is currently taken by another drive. */
export function letterTaken(row: DriveRow, rows: DriveRow[], freeLetters: string[]): boolean {
  if (row.mount) return false;
  const busyByUs = rows.some((r) => r !== row && r.mount && r.mount.state !== "failed" && r.target === row.target);
  return busyByUs || !freeLetters.includes(row.target);
}

export function newProfileId(): string {
  return `p-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}
