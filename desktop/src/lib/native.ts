// Thin wrappers over the Tauri plugins, with browser fallbacks for previews.
import { isTauri } from "./api";

export async function copyText(text: string): Promise<void> {
  if (isTauri) {
    const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
    await writeText(text);
  } else {
    await navigator.clipboard.writeText(text);
  }
}

/** Open an https URL in the default browser (the app allows a fixed list of hosts). */
export async function openUrl(url: string): Promise<void> {
  if (isTauri) {
    const opener = await import("@tauri-apps/plugin-opener");
    await opener.openUrl(url);
  } else {
    window.open(url, "_blank", "noopener,noreferrer");
  }
}

export async function pickFolder(title: string, defaultPath?: string): Promise<string | null> {
  if (!isTauri) return null;
  const { open } = await import("@tauri-apps/plugin-dialog");
  const picked = await open({ directory: true, multiple: false, title, defaultPath });
  return typeof picked === "string" ? picked : null;
}

export async function hideWindow(): Promise<void> {
  if (!isTauri) return;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  await getCurrentWindow().hide();
}

/** Subscribe to an app event emitted by the Rust side (tray menu, second launch). */
export function onAppEvent<T>(name: string, handler: (payload: T) => void): () => void {
  if (!isTauri) return () => {};
  let unlisten: (() => void) | undefined;
  let cancelled = false;
  void import("@tauri-apps/api/event").then(({ listen }) =>
    listen<T>(name, (e) => handler(e.payload)).then((u) => {
      if (cancelled) u();
      else unlisten = u;
    }),
  );
  return () => {
    cancelled = true;
    unlisten?.();
  };
}

export const HF_TOKENS_URL = "https://huggingface.co/settings/tokens";
export const WINFSP_URL = "https://winfsp.dev/rel/";
export const WINFSP_REPO_URL = "https://github.com/winfsp/winfsp";
