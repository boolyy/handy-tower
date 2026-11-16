// handy-tower: URL + token for the tower service, with a connection test.
import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands } from "@/bindings";
import type { TowerHealth } from "@/bindings";
import { SettingContainer } from "../ui/SettingContainer";
import { Input } from "../ui/Input";
import { Button } from "../ui/Button";
import { useTowerStore } from "@/stores/towerStore";

type TestResult =
  | { kind: "idle" }
  | { kind: "testing" }
  | { kind: "ok"; health: TowerHealth }
  | { kind: "error"; message: string };

export const TowerConnectionForm: React.FC<{ grouped?: boolean }> = ({
  grouped = false,
}) => {
  const { t } = useTranslation();
  const settings = useTowerStore((s) => s.settings);
  const setConnection = useTowerStore((s) => s.setConnection);
  const [url, setUrl] = useState(settings?.url ?? "");
  const [token, setToken] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [test, setTest] = useState<TestResult>({ kind: "idle" });

  useEffect(() => {
    if (settings) setUrl(settings.url);
  }, [settings?.url]);

  const dirty = !!settings && (url.trim() !== settings.url || token !== "");

  const save = async () => {
    setSaving(true);
    setError(null);
    const err = await setConnection(url, token === "" ? null : token);
    setSaving(false);
    if (err) {
      setError(err);
      return;
    }
    setToken("");
    runTest();
  };

  const runTest = async () => {
    setTest({ kind: "testing" });
    const result = await commands.towerCheckConnection();
    setTest(
      result.status === "ok"
        ? { kind: "ok", health: result.data }
        : { kind: "error", message: result.error },
    );
  };

  const testLine = () => {
    switch (test.kind) {
      case "idle":
        return null;
      case "testing":
        return (
          <p className="text-xs text-ink-3">{t("tower.connection.testing")}</p>
        );
      case "error":
        return <p className="text-xs text-error">{test.message}</p>;
      case "ok": {
        const h = test.health;
        return (
          <p className={`text-xs ${h.ok ? "text-ink-2" : "text-warning"}`}>
            {t("tower.connection.health", {
              asr: h.asr_loaded
                ? t("tower.connection.yes")
                : t("tower.connection.no"),
              mic: h.mic_present
                ? t("tower.connection.yes")
                : t("tower.connection.no"),
              llm: h.llm_reachable
                ? t("tower.connection.yes")
                : t("tower.connection.no"),
            })}
          </p>
        );
      }
    }
  };

  return (
    <>
      <SettingContainer
        title={t("tower.connection.url")}
        description={t("tower.connection.urlDescription")}
        grouped={grouped}
        layout="horizontal"
      >
        <Input
          className="w-64"
          variant="compact"
          value={url}
          placeholder={settings?.default_url}
          spellCheck={false}
          onChange={(e) => setUrl(e.target.value)}
        />
      </SettingContainer>
      <SettingContainer
        title={t("tower.connection.token")}
        description={t("tower.connection.tokenDescription")}
        grouped={grouped}
        layout="horizontal"
      >
        <Input
          className="w-64"
          variant="compact"
          type="password"
          autoComplete="off"
          value={token}
          placeholder={
            settings?.has_token
              ? t("tower.connection.tokenSaved")
              : t("tower.connection.tokenMissing")
          }
          onChange={(e) => setToken(e.target.value)}
        />
      </SettingContainer>
      <div className="px-4 p-2 flex flex-col gap-2">
        <div className="flex items-center gap-2">
          <Button size="sm" onClick={save} disabled={!dirty || saving}>
            {t("tower.connection.save")}
          </Button>
          <Button
            size="sm"
            variant="secondary"
            onClick={runTest}
            disabled={!settings?.has_token || test.kind === "testing"}
          >
            {t("tower.connection.test")}
          </Button>
          <span
            className={`ms-auto text-xs ${settings?.connected ? "text-ink-2" : "text-ink-3"}`}
          >
            {settings?.connected
              ? t("tower.connection.live")
              : t("tower.connection.offline")}
          </span>
        </div>
        {error && <p className="text-xs text-error">{error}</p>}
        {testLine()}
      </div>
    </>
  );
};
