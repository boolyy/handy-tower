// handy-tower: the tower backend's settings and connection status.
import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import { commands } from "@/bindings";
import type { TowerBackend, TowerSettingsView } from "@/bindings";
// Registers the tower strings before anything renders them.
import "@/i18n/tower";

interface TowerStore {
  settings: TowerSettingsView | null;
  load: () => Promise<TowerSettingsView>;
  /** Resolves to an error message, or null on success. */
  setBackend: (backend: TowerBackend) => Promise<string | null>;
  /** `token`: null keeps the saved one, "" clears it. */
  setConnection: (url: string, token: string | null) => Promise<string | null>;
}

export const useTowerStore = create<TowerStore>((set) => ({
  settings: null,
  load: async () => {
    const settings = await commands.getTowerSettings();
    set({ settings });
    return settings;
  },
  setBackend: async (backend) => {
    const result = await commands.setTowerBackend(backend);
    if (result.status === "ok") {
      set({ settings: result.data });
      return null;
    }
    return result.error;
  },
  setConnection: async (url, token) => {
    const result = await commands.setTowerConnection(url, token);
    if (result.status === "ok") {
      set({ settings: result.data });
      return null;
    }
    return result.error;
  },
}));

/** True while the tower backend is selected. */
export const useTowerMode = () =>
  useTowerStore((s) => s.settings?.backend === "tower");

/** Non-hook read, for code that runs outside React's render. */
export const isTowerMode = () =>
  useTowerStore.getState().settings?.backend === "tower";

// The backend emits `tower-status` when the event stream connects or drops.
let listening = false;
export const startTowerStatusListener = () => {
  if (listening) return;
  listening = true;
  listen<boolean>("tower-status", (event) => {
    useTowerStore.setState((s) =>
      s.settings
        ? { settings: { ...s.settings, connected: event.payload } }
        : s,
    );
  });
};
