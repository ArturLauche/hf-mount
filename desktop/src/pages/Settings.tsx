import { Copy, ExternalLink, FolderOpen, Monitor, Moon, Sun } from "lucide-react";
import { useEffect, useState } from "react";
import { PageHeader, Select, SettingsGroup, SettingsRow } from "../components/common";
import { useToast } from "../components/ui/overlay";
import { Button, Input, Segmented, Spinner, Switch } from "../components/ui/primitives";
import { api, errorInfo } from "../lib/api";
import { formatBytes } from "../lib/format";
import { copyText, openUrl, pickFolder, WINFSP_REPO_URL } from "../lib/native";
import { useApp } from "../lib/state";
import type { Settings, Theme } from "../lib/types";

const GB = 1_000_000_000;
const CACHE_SIZES = [2, 5, 10, 25, 50, 100, 250].map((n) => ({ value: n * GB, label: `${n} GB` }));
const UPLOAD_WAITS = [
  { value: 60, label: "1 minute" },
  { value: 300, label: "5 minutes" },
  { value: 900, label: "15 minutes" },
  { value: 3600, label: "1 hour" },
];
const TTLS = [
  { value: 2_000, label: "2 seconds" },
  { value: 10_000, label: "10 seconds" },
  { value: 60_000, label: "1 minute" },
  { value: 600_000, label: "10 minutes" },
];

export function SettingsPage() {
  const { settings, updateSettings, appInfo, overview } = useApp();
  const toast = useToast();

  const set = async <K extends keyof Settings>(key: K, value: Settings[K]) => {
    if (!settings) return;
    try {
      await updateSettings({ ...settings, [key]: value });
    } catch (e) {
      toast({ tone: "error", title: "Setting not saved", body: errorInfo(e).message });
    }
  };

  if (!settings) {
    return (
      <>
        <PageHeader title="Settings" />
        <div className="flex items-center gap-2 text-[13px] text-fg-3" role="status">
          <Spinner /> Loading…
        </div>
      </>
    );
  }

  const withCurrent = <T extends number>(opts: { value: T; label: string }[], current: T, fmt: (v: T) => string) =>
    opts.some((o) => o.value === current) ? opts : [...opts, { value: current, label: fmt(current) }].sort((a, b) => a.value - b.value);

  return (
    <>
      <PageHeader title="Settings" description="Changes to cache and network options apply to drives mounted afterwards." />
      <div className="flex flex-col gap-7">
        <SettingsGroup title="Appearance">
          <SettingsRow label="Theme" description="Follow Windows, or always use light or dark.">
            <Segmented<Theme>
              size="sm"
              label="Theme"
              value={settings.theme}
              onChange={(t) => void set("theme", t)}
              options={[
                { value: "system", label: "System", icon: <Monitor className="size-3.5" /> },
                { value: "light", label: "Light", icon: <Sun className="size-3.5" /> },
                { value: "dark", label: "Dark", icon: <Moon className="size-3.5" /> },
              ]}
            />
          </SettingsRow>
        </SettingsGroup>

        <SettingsGroup title="Startup and closing">
          <Switch
            label="Start hf-mount when I sign in to Windows"
            description="Starts minimized to the tray."
            checked={settings.launch_at_login}
            onCheckedChange={(v) => void set("launch_at_login", v)}
          />
          <Switch
            label="Mount saved drives when hf-mount starts"
            description="Only drives marked “Mount at startup”."
            checked={settings.reconnect_on_start}
            onCheckedChange={(v) => void set("reconnect_on_start", v)}
          />
          <Switch
            label="Keep running in the tray when the window is closed"
            description="Drives stay mounted either way; this only controls the tray icon."
            checked={settings.close_to_tray}
            onCheckedChange={(v) => void set("close_to_tray", v)}
          />
          <Switch
            label="Unmount all drives when quitting the app"
            description="Otherwise drives keep working after the app quits, until you unmount them or sign out of Windows."
            checked={settings.unmount_on_quit}
            onCheckedChange={(v) => void set("unmount_on_quit", v)}
          />
        </SettingsGroup>

        <SettingsGroup title="Cache" description="Downloaded file parts are kept on this PC so repeated reads are fast.">
          <SettingsRow label="Location" description={<span className="selectable font-mono text-[12px]">{settings.cache_dir ?? appInfo?.cache_dir ?? "Default"}</span>}>
            {settings.cache_dir && (
              <Button size="sm" variant="ghost" onClick={() => void set("cache_dir", null)}>
                Reset
              </Button>
            )}
            <Button
              size="sm"
              onClick={async () => {
                const dir = await pickFolder("Choose the cache folder", settings.cache_dir ?? appInfo?.cache_dir);
                if (dir) await set("cache_dir", dir);
              }}
            >
              Change…
            </Button>
            <Button size="sm" variant="ghost" icon={<FolderOpen className="size-3.5" />} onClick={() => void api.openPath("cache")}>
              Open
            </Button>
          </SettingsRow>
          <SettingsRow label="Maximum size" htmlFor="cache-size" description="Per drive. Older data is removed when the limit is reached.">
            <Select
              id="cache-size"
              className="w-[140px]"
              value={settings.cache_size_bytes}
              onChange={(v) => void set("cache_size_bytes", v)}
              options={withCurrent(CACHE_SIZES, settings.cache_size_bytes, formatBytes)}
            />
          </SettingsRow>
        </SettingsGroup>

        <SettingsGroup title="Buckets and network">
          <SettingsRow
            label="Wait for uploads when unmounting"
            htmlFor="upload-wait"
            description="If uploads take longer, unsaved copies are kept in the recovered files folder."
          >
            <Select
              id="upload-wait"
              className="w-[140px]"
              value={settings.upload_wait_secs}
              onChange={(v) => void set("upload_wait_secs", v)}
              options={withCurrent(UPLOAD_WAITS, settings.upload_wait_secs, (v) => `${v} seconds`)}
            />
          </SettingsRow>
          <SettingsRow label="Check for remote changes" htmlFor="ttl" description="How long file listings are reused before asking Hugging Face again.">
            <Select
              id="ttl"
              className="w-[140px]"
              value={settings.metadata_ttl_ms}
              onChange={(v) => void set("metadata_ttl_ms", v)}
              options={withCurrent(TTLS, settings.metadata_ttl_ms, (v) => `${v / 1000} seconds`)}
            />
          </SettingsRow>
          <HubEndpoint value={settings.hub_endpoint} onSave={(v) => set("hub_endpoint", v)} />
        </SettingsGroup>

        <SettingsGroup title="Command line">
          <SettingsRow
            label="hf-mount.exe"
            description={
              <span>
                Use <code className="font-mono text-[12px] text-fg-2">hf-mount status</code> in a terminal to see the same drives.
                <span className="selectable mt-0.5 block font-mono text-[12px]">{appInfo?.cli_path ?? "Not found next to the app"}</span>
              </span>
            }
          >
            {appInfo?.cli_path && (
              <Button
                size="sm"
                icon={<Copy className="size-3.5" />}
                onClick={() => void copyText(appInfo.cli_path!).then(() => toast({ tone: "success", title: "Path copied" }))}
              >
                Copy path
              </Button>
            )}
          </SettingsRow>
        </SettingsGroup>

        <SettingsGroup title="About">
          <SettingsRow label="hf-mount" description="Licensed under the Apache License 2.0.">
            <span className="font-mono text-[12.5px] text-fg-2">{appInfo?.version ?? "…"}</span>
          </SettingsRow>
          <SettingsRow
            label="WinFsp"
            description={
              <span>
                WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos. Used unmodified under the GPLv3 with the
                FLOSS exception.
              </span>
            }
          >
            <span className="font-mono text-[12.5px] text-fg-2">
              {overview?.driver.installed ? (overview.driver.version ?? "installed") : "not installed"}
            </span>
            <Button size="sm" variant="ghost" icon={<ExternalLink className="size-3.5" />} onClick={() => void openUrl(WINFSP_REPO_URL)}>
              Source
            </Button>
          </SettingsRow>
          <SettingsRow label="Data folder" description={<span className="selectable font-mono text-[12px]">{appInfo?.home_dir ?? ""}</span>}>
            <Button size="sm" variant="ghost" icon={<FolderOpen className="size-3.5" />} onClick={() => void api.openPath("home")}>
              Open
            </Button>
          </SettingsRow>
        </SettingsGroup>
      </div>
    </>
  );
}

function HubEndpoint({ value, onSave }: { value: string | null; onSave: (v: string | null) => Promise<void> }) {
  const [text, setText] = useState(value ?? "");
  const [problem, setProblem] = useState<string | null>(null);
  useEffect(() => setText(value ?? ""), [value]);

  const commit = async () => {
    const t = text.trim().replace(/\/+$/, "");
    if (!t) {
      setProblem(null);
      if (value !== null) await onSave(null);
      return;
    }
    if (!/^https:\/\/[^\s/]+(\/\S*)?$/.test(t)) {
      setProblem("Use an https:// address");
      return;
    }
    setProblem(null);
    if (t !== value) await onSave(t);
  };

  return (
    <SettingsRow
      label="Hub address"
      htmlFor="hub"
      description={problem ? <span className="text-danger">{problem}</span> : "Leave empty for huggingface.co."}
    >
      <Input
        id="hub"
        mono
        className="h-9 w-[240px]"
        value={text}
        placeholder="https://huggingface.co"
        invalid={!!problem}
        onChange={(e) => setText(e.target.value)}
        onBlur={() => void commit()}
        onKeyDown={(e) => e.key === "Enter" && void commit()}
      />
    </SettingsRow>
  );
}
