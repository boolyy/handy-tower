// handy-tower: Settings > Tower. Backend choice, connection and hotkeys.
import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { type } from "@tauri-apps/plugin-os";
import type { TowerBackend } from "@/bindings";
import { SettingsGroup } from "../ui/SettingsGroup";
import { SettingContainer } from "../ui/SettingContainer";
import { Dropdown } from "../ui/Dropdown";
import { ShortcutInput } from "../settings/ShortcutInput";
import { HoldThreshold } from "../settings/debug/HoldThreshold";
import { TowerConnectionForm } from "./TowerConnectionForm";
import { useTowerStore } from "@/stores/towerStore";

export const TowerSettings: React.FC = () => {
  const { t } = useTranslation();
  const settings = useTowerStore((s) => s.settings);
  const setBackend = useTowerStore((s) => s.setBackend);
  const [error, setError] = useState<string | null>(null);
  const isLinux = type() === "linux";
  const tower = settings?.backend === "tower";

  const choose = async (value: string) => {
    setError(await setBackend(value as TowerBackend));
  };

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup
        title={t("tower.settings.title")}
        description={t("tower.settings.description")}
      >
        <SettingContainer
          title={t("tower.backend.title")}
          description={t("tower.backend.description")}
          grouped
        >
          <Dropdown
            selectedValue={settings?.backend ?? null}
            onSelect={choose}
            options={[
              { value: "tower", label: t("tower.backend.tower") },
              { value: "local", label: t("tower.backend.local") },
            ]}
          />
        </SettingContainer>
        {error && <p className="px-4 pb-2 text-xs text-error">{error}</p>}
        {tower && <TowerConnectionForm grouped />}
      </SettingsGroup>

      {tower && (
        <SettingsGroup
          title={t("tower.hotkeys.title")}
          description={t("tower.hotkeys.description")}
        >
          <ShortcutInput shortcutId="tower_dictate" grouped />
          <ShortcutInput shortcutId="tower_raw" grouped />
          <HoldThreshold grouped />
          {/* Esc is only grabbed while the tower is recording or holding text. */}
          {!isLinux && <ShortcutInput shortcutId="cancel" grouped />}
        </SettingsGroup>
      )}

      {!tower && (
        <p className="px-4 text-xs text-ink-3">
          {t("tower.backend.localNote")}
        </p>
      )}
    </div>
  );
};
