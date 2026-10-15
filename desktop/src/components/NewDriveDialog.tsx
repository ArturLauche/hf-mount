import * as Collapsible from "@radix-ui/react-collapsible";
import { AlertCircle, ChevronRight, Info } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { errorInfo } from "../lib/api";
import { errorAction, errorTitle, sentence } from "../lib/errors";
import { idProblem, parseSourceInput, revisionProblem, SOURCE_TYPES, subfolderProblem } from "../lib/source";
import type { AccountStatus, ErrorInfo, MountInfo, MountSpec, Profile, SourceType } from "../lib/types";
import { Select } from "./common";
import { Dialog } from "./ui/overlay";
import { Button, cn, Field, Input, Kbd, Label, Segmented, Switch } from "./ui/primitives";

export interface NewDriveInitial {
  source_type?: SourceType;
  id?: string;
}

export interface MountRequest {
  spec: MountSpec;
  save: boolean;
  autoMount: boolean;
}

const LOOKS_LIKE_SOURCE = /huggingface\.co|^hf:\/\/|^(datasets|spaces|buckets|models)\//i;

const PLACEHOLDER: Record<SourceType, string> = {
  model: "openai-community/gpt2",
  dataset: "HuggingFaceFW/fineweb-edu",
  space: "gradio/hello_world",
  bucket: "your-name/bucket-name",
};

/** Pick the default letter: the highest free letter not reserved by a saved drive. */
export function defaultLetter(free: string[], profiles: Profile[]): string {
  const reserved = new Set(profiles.map((p) => p.spec.target));
  const preferred = free.filter((l) => !reserved.has(l));
  return preferred[preferred.length - 1] ?? free[free.length - 1] ?? "";
}

export function NewDriveDialog({ open, onOpenChange, initial, freeLetters, profiles, account, driverInstalled, onSubmit, onSignIn, onInstallDriver }: {
  open: boolean;
  onOpenChange: (o: boolean) => void;
  initial?: NewDriveInitial;
  freeLetters: string[];
  profiles: Profile[];
  account: AccountStatus | null;
  driverInstalled: boolean;
  onSubmit: (req: MountRequest) => Promise<MountInfo>;
  onSignIn: () => void;
  onInstallDriver: () => void;
}) {
  const [type, setType] = useState<SourceType>("model");
  const [idText, setIdText] = useState("");
  const [revision, setRevision] = useState("");
  const [subfolder, setSubfolder] = useState("");
  const [target, setTarget] = useState("");
  const [writable, setWritable] = useState(true);
  const [save, setSave] = useState(true);
  const [autoMount, setAutoMount] = useState(false);
  const [advanced, setAdvanced] = useState(false);
  const [touched, setTouched] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<ErrorInfo | null>(null);
  const idRef = useRef<HTMLInputElement>(null);
  const letterRef = useRef<HTMLDivElement>(null);

  // Reset each time the dialog opens.
  useEffect(() => {
    if (!open) return;
    setType(initial?.source_type ?? "model");
    setIdText(initial?.id ?? "");
    setRevision("");
    setSubfolder("");
    setTarget(defaultLetter(freeLetters, profiles));
    setWritable(true);
    setSave(true);
    setAutoMount(false);
    setAdvanced(false);
    setTouched(false);
    setSubmitting(false);
    setError(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only on open
  }, [open]);

  // Keep the chosen letter valid as drives come and go.
  useEffect(() => {
    if (open && !submitting && target && !freeLetters.includes(target)) setTarget(defaultLetter(freeLetters, profiles));
  }, [open, submitting, target, freeLetters, profiles]);

  /**
   * Pasted Hub URLs and prefixed ids (`datasets/…`, `hf://buckets/…`) fill in
   * the type, revision and folder. Applied on paste and when the field loses
   * focus, never while typing, so typing a URL by hand is not mangled.
   */
  const applySourceText = (text: string): string => {
    if (!LOOKS_LIKE_SOURCE.test(text.trim())) return text;
    const p = parseSourceInput(text);
    if (p.source_type) setType(p.source_type);
    setIdText(p.id);
    if (p.revision) setRevision(p.revision);
    if (p.subfolder) setSubfolder(p.subfolder);
    if (p.revision || p.subfolder) setAdvanced(true);
    return p.id;
  };

  const id = idText.trim();
  const isBucket = type === "bucket";
  const problems = {
    id: idProblem(type, id),
    revision: isBucket ? null : revisionProblem(revision),
    subfolder: subfolderProblem(subfolder),
    target: target ? null : "No drive letter is free",
  };
  const invalid = Object.values(problems).some(Boolean);

  const reservedBy = useMemo(() => {
    const map = new Map<string, string>();
    for (const p of profiles) map.set(p.spec.target, p.name);
    return map;
  }, [profiles]);

  const letterOptions = freeLetters.map((l) => ({
    value: l,
    label: reservedBy.has(l) ? `${l}  — saved for ${reservedBy.get(l)}` : l,
  }));

  const signedIn = account?.state === "signed_in" || account?.state === "unverified";

  const submit = async () => {
    setTouched(true);
    if (LOOKS_LIKE_SOURCE.test(id)) {
      // A URL typed by hand and submitted with Enter: split it first and let
      // the person review the filled-in fields.
      try {
        applySourceText(idText);
      } catch (e) {
        setError(errorInfo(e));
      }
      return;
    }
    if (invalid || submitting) {
      if (problems.id) idRef.current?.focus();
      return;
    }
    const spec: MountSpec = {
      source_type: type,
      id,
      subfolder: subfolder.trim().replace(/^\/+|\/+$/g, "") || null,
      revision: isBucket ? null : revision.trim() || null,
      target,
      read_only: !isBucket || !writable,
    };
    setSubmitting(true);
    setError(null);
    try {
      await onSubmit({ spec, save, autoMount: save && autoMount });
      onOpenChange(false);
    } catch (e) {
      const info = errorInfo(e);
      setError(info);
      if (errorAction(info) === "pick_letter") letterRef.current?.querySelector("select")?.focus();
    } finally {
      setSubmitting(false);
    }
  };

  const action = error ? errorAction(error) : "none";

  return (
    <Dialog
      open={open}
      onOpenChange={(o) => !submitting && onOpenChange(o)}
      title="New drive"
      description="Mount a Hugging Face repository or bucket as a Windows drive. Files download on demand."
      width={560}
      footer={
        <>
          <span className="mr-auto text-[12px] text-fg-3 max-[560px]:hidden">
            <Kbd>Enter</Kbd> to mount
          </span>
          <Button variant="ghost" onClick={() => onOpenChange(false)} disabled={submitting}>
            Cancel
          </Button>
          <Button variant="primary" onClick={() => void submit()} loading={submitting} disabled={!driverInstalled}>
            {submitting ? "Mounting…" : `Mount as ${target || "drive"}`}
          </Button>
        </>
      }
    >
      <form
        className="flex flex-col gap-5 pb-2"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <div className="flex flex-col gap-1.5">
          <Label>Type</Label>
          <Segmented
            label="Source type"
            value={type}
            onChange={(t) => {
              setType(t);
              setError(null);
            }}
            options={SOURCE_TYPES.map((t) => ({ value: t.value, label: t.label }))}
          />
        </div>

        <Field
          label={isBucket ? "Bucket" : "Repository"}
          htmlFor="nd-id"
          error={touched ? problems.id : null}
          hint="Paste an id or a huggingface.co link."
        >
          <Input
            id="nd-id"
            ref={idRef}
            mono
            autoFocus
            value={idText}
            placeholder={PLACEHOLDER[type]}
            invalid={touched && !!problems.id}
            onChange={(e) => {
              setIdText(e.target.value);
              setError(null);
            }}
            onPaste={(e) => {
              const pasted = e.clipboardData.getData("text");
              const input = e.currentTarget;
              const next = input.value.slice(0, input.selectionStart ?? 0) + pasted + input.value.slice(input.selectionEnd ?? 0);
              if (applySourceText(next) !== next) e.preventDefault();
            }}
            onBlur={() => applySourceText(idText)}
            disabled={submitting}
          />
        </Field>

        <div className="grid grid-cols-[140px_1fr] items-start gap-4 max-[520px]:grid-cols-1">
          <div ref={letterRef} className="flex flex-col gap-1.5">
            <Label htmlFor="nd-letter">Drive letter</Label>
            <Select
              id="nd-letter"
              value={target}
              onChange={(v) => {
                setTarget(v);
                if (error && errorAction(error) === "pick_letter") setError(null);
              }}
              options={letterOptions.length ? letterOptions : [{ value: "", label: "None free", disabled: true }]}
            />
          </div>
          <div className="pt-6 text-[12.5px] leading-relaxed text-fg-3 max-[520px]:pt-0">
            {isBucket
              ? writable
                ? "Changes upload in the background after you close a file. Unmounting waits for uploads to finish."
                : "The bucket is mounted read-only."
              : "Repositories are always read-only. Data is cached on this PC and shared between drives."}
          </div>
        </div>

        {isBucket && (
          <div className="-my-2">
            <Switch
              checked={writable}
              onCheckedChange={setWritable}
              label="Allow changes"
              description="Create, edit, rename and delete files in the bucket."
              disabled={submitting}
            />
          </div>
        )}

        <Collapsible.Root open={advanced} onOpenChange={setAdvanced}>
          <Collapsible.Trigger asChild>
            <button
              type="button"
              className="-ml-1 inline-flex items-center gap-1 rounded-[8px] px-1 py-0.5 text-[13px] font-medium text-fg-2 hover:text-fg"
            >
              <ChevronRight className={cn("size-4 transition-transform duration-150", advanced && "rotate-90")} aria-hidden />
              Advanced
            </button>
          </Collapsible.Trigger>
          <Collapsible.Content className="anim-fade">
            <div className={cn("mt-3 grid gap-4", isBucket ? "grid-cols-1" : "grid-cols-2 max-[520px]:grid-cols-1")}>
              {!isBucket && (
                <Field label="Revision" htmlFor="nd-rev" error={problems.revision} hint="Branch, tag or commit. Default: main.">
                  <Input
                    id="nd-rev"
                    mono
                    value={revision}
                    placeholder="main"
                    invalid={!!problems.revision}
                    onChange={(e) => setRevision(e.target.value)}
                    disabled={submitting}
                  />
                </Field>
              )}
              <Field label="Folder" htmlFor="nd-sub" error={problems.subfolder} hint="Mount only this folder.">
                <Input
                  id="nd-sub"
                  mono
                  value={subfolder}
                  placeholder="e.g. onnx"
                  invalid={!!problems.subfolder}
                  onChange={(e) => setSubfolder(e.target.value)}
                  disabled={submitting}
                />
              </Field>
            </div>
          </Collapsible.Content>
        </Collapsible.Root>

        <div className="divide-y divide-border rounded-[14px] border border-border px-4">
          <Switch
            checked={save}
            onCheckedChange={setSave}
            label="Save to my drives"
            description="Keep it in the list so you can mount it again with one click."
            disabled={submitting}
          />
          <Switch
            checked={save && autoMount}
            onCheckedChange={setAutoMount}
            label="Mount when hf-mount starts"
            disabled={!save || submitting}
          />
        </div>

        {!signedIn && isBucket && !error && (
          <p className="flex items-start gap-2 text-[12.5px] text-fg-2">
            <Info className="mt-0.5 size-3.5 shrink-0 text-fg-3" aria-hidden />
            <span>
              Buckets and private repositories need a Hugging Face token.{" "}
              <button type="button" className="font-medium text-fg underline underline-offset-2" onClick={onSignIn}>
                Sign in
              </button>
            </span>
          </p>
        )}

        {!driverInstalled && (
          <ErrorBox
            info={{ code: "driver_missing", message: "WinFsp is needed to create drives", hint: null }}
            actionLabel="Install WinFsp"
            onAction={onInstallDriver}
          />
        )}

        {error && (
          <ErrorBox
            info={error}
            actionLabel={action === "sign_in" ? (signedIn ? "Change token" : "Sign in") : action === "install_driver" ? "Install WinFsp" : undefined}
            onAction={action === "sign_in" ? onSignIn : action === "install_driver" ? onInstallDriver : undefined}
          />
        )}
        <button type="submit" hidden aria-hidden tabIndex={-1} />
      </form>
    </Dialog>
  );
}

function ErrorBox({ info, actionLabel, onAction }: { info: ErrorInfo; actionLabel?: string; onAction?: () => void }) {
  return (
    <div role="alert" className="anim-fade flex flex-wrap items-start gap-3 rounded-[14px] bg-danger-soft px-4 py-3">
      <AlertCircle className="mt-0.5 size-4 shrink-0 text-danger" aria-hidden />
      <div className="min-w-[200px] flex-1 text-[13px]">
        <p className="font-medium text-fg">{errorTitle(info)}</p>
        <p className="selectable text-fg-2">{sentence(info.message)}</p>
        {info.hint && <p className="mt-0.5 text-fg-3">{sentence(info.hint)}</p>}
      </div>
      {actionLabel && onAction && (
        <Button size="sm" onClick={onAction} className="self-center">
          {actionLabel}
        </Button>
      )}
    </div>
  );
}
