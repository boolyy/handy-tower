// handy-tower: first run (and returning-user check) for the tower backend.
// No model download and no microphone: just the connection, and on macOS the
// Accessibility grant that pasting needs.
import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { platform } from "@tauri-apps/plugin-os";
import {
  checkAccessibilityPermission,
  requestAccessibilityPermission,
} from "tauri-plugin-macos-permissions-api";
import { commands } from "@/bindings";
import { Button } from "../ui/Button";
import { SettingsGroup } from "../ui/SettingsGroup";
import { TowerConnectionForm } from "./TowerConnectionForm";
import { useTowerStore } from "@/stores/towerStore";

interface TowerSetupProps {
  onComplete: () => void;
  /** The user picked local models instead: run upstream's onboarding. */
  onUseLocal: () => void;
}

export const TowerSetup: React.FC<TowerSetupProps> = ({
  onComplete,
  onUseLocal,
}) => {
  const { t } = useTranslation();
  const settings = useTowerStore((s) => s.settings);
  const setBackend = useTowerStore((s) => s.setBackend);
  const isMac = platform() === "macos";
  const [accessibility, setAccessibility] = useState<boolean | null>(
    isMac ? null : true,
  );
  const poll = useRef<ReturnType<typeof setInterval>>();

  useEffect(() => {
    if (!isMac) return;
    const check = () =>
      checkAccessibilityPermission()
        .then((granted) => {
          setAccessibility(granted);
          if (granted && poll.current) {
            clearInterval(poll.current);
            poll.current = undefined;
          }
        })
        .catch(() => setAccessibility(false));
    check();
    poll.current = setInterval(check, 1000);
    return () => clearInterval(poll.current);
  }, [isMac]);

  const grant = async () => {
    try {
      // An unsigned update keeps a stale entry that looks enabled but no
      // longer matches this build; clear it before asking again.
      await commands.resetMacosPermission("accessibility");
      await requestAccessibilityPermission();
    } catch (e) {
      console.error("Failed to request Accessibility:", e);
    }
  };

  const finish = async () => {
    await commands.completeOnboarding();
    onComplete();
  };

  const useLocal = async () => {
    if ((await setBackend("local")) === null) onUseLocal();
  };

  const ready = !!settings?.has_token && accessibility === true;

  return (
    <div className="h-screen overflow-y-auto select-none cursor-default">
      <div className="max-w-xl mx-auto p-6 space-y-6">
        <div className="space-y-2">
          <h1 className="text-xl font-semibold">{t("tower.setup.title")}</h1>
          <p className="text-sm text-ink-2">{t("tower.setup.intro")}</p>
        </div>

        <SettingsGroup title={t("tower.setup.connection")}>
          <TowerConnectionForm grouped />
        </SettingsGroup>

        {isMac && (
          <SettingsGroup title={t("tower.setup.accessibility")}>
            <div className="px-4 p-3 flex items-center gap-3">
              <p className="text-sm flex-1">
                {accessibility
                  ? t("tower.setup.accessibilityGranted")
                  : t("tower.setup.accessibilityNeeded")}
              </p>
              {!accessibility && (
                <Button size="sm" onClick={grant}>
                  {t("tower.setup.grant")}
                </Button>
              )}
            </div>
          </SettingsGroup>
        )}

        <div className="flex items-center gap-3">
          <Button onClick={finish} disabled={!ready}>
            {t("tower.setup.continue")}
          </Button>
          <Button variant="ghost" size="sm" onClick={useLocal}>
            {t("tower.setup.useLocal")}
          </Button>
        </div>
      </div>
    </div>
  );
};
