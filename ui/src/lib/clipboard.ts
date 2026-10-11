import { api } from "./api";

/**
 * Copy text to the clipboard. Goes through Rust (the native OS clipboard),
 * which behaves the same on macOS, Windows and Linux; the web-API fallback
 * only matters when running the UI outside Tauri (e.g. `vite preview`).
 */
export async function copyText(text: string): Promise<boolean> {
  try {
    await api.copyText(text);
    return true;
  } catch {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      return false;
    }
  }
}
