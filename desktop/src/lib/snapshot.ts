/** Compare decoded IPC data without serializing the previous and next responses. */
export function sameSnapshot(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (a === null || b === null || typeof a !== "object" || typeof b !== "object") return false;
  if (Array.isArray(a) || Array.isArray(b)) {
    return Array.isArray(a) && Array.isArray(b) && a.length === b.length && a.every((item, i) => sameSnapshot(item, b[i]));
  }
  const left = a as Record<string, unknown>;
  const right = b as Record<string, unknown>;
  for (const key in left) {
    if (!Object.hasOwn(right, key) || !sameSnapshot(left[key], right[key])) return false;
  }
  for (const key in right) {
    if (!Object.hasOwn(left, key)) return false;
  }
  return true;
}
