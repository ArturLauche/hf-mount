import type { MountSpec, SourceType } from "./types";

export const SOURCE_TYPES: { value: SourceType; label: string; hint: string }[] = [
  { value: "model", label: "Model", hint: "Read-only repository" },
  { value: "dataset", label: "Dataset", hint: "Read-only repository" },
  { value: "space", label: "Space", hint: "Read-only repository" },
  { value: "bucket", label: "Bucket", hint: "Read & write storage" },
];

export interface ParsedSource {
  source_type?: SourceType;
  id: string;
  subfolder?: string;
  revision?: string;
}

/**
 * Accept what people paste: `user/name`, `datasets/user/name`, Hub URLs
 * (`https://huggingface.co/datasets/user/name/tree/main/sub`) and
 * `hf://buckets/user/name`. Returns the pieces; validation is separate.
 */
export function parseSourceInput(raw: string): ParsedSource {
  let text = raw.trim();
  let source_type: SourceType | undefined;
  let revision: string | undefined;
  let subfolder: string | undefined;

  const url = text.match(/^(?:https?:\/\/)?(?:www\.)?huggingface\.co\/(.*)$/i);
  if (url) text = url[1] ?? "";
  const hf = text.match(/^hf:\/\/(.*)$/i);
  if (hf) text = hf[1] ?? "";
  text = text.replace(/[?#].*$/, "").replace(/^\/+|\/+$/g, "");

  const prefixes: [string, SourceType][] = [
    ["datasets/", "dataset"],
    ["spaces/", "space"],
    ["buckets/", "bucket"],
    ["models/", "model"],
  ];
  for (const [prefix, type] of prefixes) {
    if (text.toLowerCase().startsWith(prefix)) {
      source_type = type;
      text = text.slice(prefix.length);
      break;
    }
  }

  const parts = text.split("/").filter((p) => p.length > 0);
  let idParts = parts.slice(0, 2);
  let rest = parts.slice(2);
  // Hub browser URLs: <id>/tree/<rev>/<path> or <id>/blob/<rev>/<path>
  if ((rest[0] === "tree" || rest[0] === "blob" || rest[0] === "resolve") && rest.length >= 2) {
    revision = decodeURIComponent(rest[1]!);
    rest = rest.slice(2);
  }
  if (rest.length > 0) subfolder = rest.map(decodeURIComponent).join("/");
  if (idParts.length === 1 && parts.length === 1) idParts = [parts[0]!];
  return { source_type, id: idParts.join("/"), subfolder, revision };
}

const SEGMENT = /^[A-Za-z0-9](?:[A-Za-z0-9._-]{0,94}[A-Za-z0-9])?$/;

/** Mirrors `validate_name_segment` in src/manager/spec.rs. */
function segmentProblem(segment: string, what: string): string | null {
  if (!segment) return `${what} is missing`;
  if (segment === "." || segment === "..") return "ids must not contain '.' or '..'";
  if (segment.length > 96) return `${what} is longer than 96 characters`;
  if (/[^A-Za-z0-9._-]/.test(segment)) return `${what} may only contain letters, digits, '-', '_' and '.'`;
  if (!SEGMENT.test(segment)) return `${what} must not start or end with '-' or '.'`;
  if (segment.includes("--") || segment.includes("..")) return `${what} must not contain '--' or '..'`;
  return null;
}

export function idProblem(type: SourceType, id: string): string | null {
  const parts = id.split("/");
  if (!id.trim()) return type === "bucket" ? "Enter a bucket id like namespace/bucket" : "Enter a repository id like namespace/name";
  if (type === "bucket") {
    if (parts.length !== 2) return "Bucket ids look like namespace/bucket-name";
  } else if (parts.length > 2) {
    return "Repository ids look like namespace/name";
  }
  const names = parts.length === 2 ? [parts[0]!, parts[1]!] : [parts[0]!];
  const labels = parts.length === 2 ? ["Namespace", type === "bucket" ? "Bucket name" : "Name"] : ["Name"];
  for (let i = 0; i < names.length; i++) {
    const p = segmentProblem(names[i]!, labels[i]!);
    if (p) return p;
  }
  return null;
}

export function subfolderProblem(sub: string): string | null {
  const t = sub.trim().replace(/^\/+|\/+$/g, "");
  if (!t) return null;
  if (t.includes("\\")) return "Use '/' to separate folders";
  for (const c of t.split("/")) {
    if (!c || c === "." || c === "..") return "Folders must not be empty, '.' or '..'";
    if (/[?#:]/.test(c) || /\p{Cc}/u.test(c)) return "Folder names contain an invalid character";
  }
  return null;
}

/** Mirrors `validate_revision` in src/setup.rs. */
export function revisionProblem(rev: string): string | null {
  const t = rev.trim();
  if (!t) return null;
  if (/[^A-Za-z0-9._/-]/.test(t)) return "Revisions may contain letters, digits, '-', '_', '.' and '/'";
  if (t.includes("..")) return "Revisions must not contain '..'";
  return null;
}

export function typeLabel(t: SourceType): string {
  return { model: "Model", dataset: "Dataset", space: "Space", bucket: "Bucket" }[t];
}

/** `openai-community/gpt2@main · /sub` */
export function sourceLine(spec: MountSpec): string {
  let s = spec.id;
  if (spec.source_type !== "bucket") s += `@${spec.revision ?? "main"}`;
  if (spec.subfolder) s += ` · /${spec.subfolder}`;
  return s;
}

export function displayName(spec: MountSpec): string {
  const name = spec.id.split("/").pop() ?? spec.id;
  return spec.subfolder ? `${name}/${spec.subfolder.split("/").pop()}` : name;
}

export function hubUrl(spec: MountSpec): string {
  const prefix = { model: "", dataset: "datasets/", space: "spaces/", bucket: "buckets/" }[spec.source_type];
  return `https://huggingface.co/${prefix}${spec.id}`;
}
