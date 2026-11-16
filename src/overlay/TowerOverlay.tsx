// handy-tower: the overlay while the tower backend is active. The backend
// sends `tower-overlay` payloads (src-tauri/src/tower/overlay.rs) and shows the
// window in the "tower" state; this draws one compact row. Never shows text.
import { listen } from "@tauri-apps/api/event";
import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { commands } from "@/bindings";
import { NoticeBadgeIcon } from "@/components/icons/NoticeBadgeIcon";
import "@/i18n/tower";
import "./TowerOverlay.css";

export type TowerMessage =
  | "busy"
  | "no_speech"
  | "cleanup_failed"
  | "unreachable"
  | "unauthorized"
  | "no_token"
  | "mic_unavailable"
  | "timeout"
  | "server_error";

export interface TowerOverlayPayload {
  kind: "recording" | "held" | "finishing" | "message";
  elapsed_ms: number;
  raw: boolean;
  message: TowerMessage | null;
  error: boolean;
}

/** The latest payload and when it arrived (to tick the timer locally). */
export const useTowerOverlay = () => {
  const [state, setState] = useState<{
    payload: TowerOverlayPayload | null;
    receivedAt: number;
  }>({ payload: null, receivedAt: 0 });

  useEffect(() => {
    const unlisten = listen<TowerOverlayPayload>("tower-overlay", (event) => {
      setState({ payload: event.payload, receivedAt: Date.now() });
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  return state;
};

const fmtTime = (ms: number) => {
  const s = Math.floor(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
};

interface TowerCardProps {
  payload: TowerOverlayPayload | null;
  receivedAt: number;
  isVisible: boolean;
  position: "top" | "bottom";
  direction: "ltr" | "rtl";
}

export const TowerCard: React.FC<TowerCardProps> = ({
  payload,
  receivedAt,
  isVisible,
  position,
  direction,
}) => {
  const { t } = useTranslation();
  const [now, setNow] = useState(Date.now());
  const recording = payload?.kind === "recording";

  useEffect(() => {
    if (!recording || !isVisible) return;
    setNow(Date.now());
    const id = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(id);
  }, [recording, isVisible, receivedAt]);

  if (!payload) return null;

  const cancelBtn = (
    <button
      className="sx"
      aria-label="cancel"
      onClick={() => commands.cancelOperation()}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true">
        <path
          d="M4 4 L12 12 M12 4 L4 12"
          stroke="currentColor"
          strokeWidth="1.6"
          strokeLinecap="round"
        />
      </svg>
    </button>
  );

  let row: React.ReactNode;
  let cardClass = "";
  switch (payload.kind) {
    case "recording":
      row = (
        <div className="sbase">
          <div className="sbase-l">
            <span className="sdot ready" />
          </div>
          <span className="swork-label">
            {payload.raw
              ? t("tower.overlay.recordingRaw")
              : t("tower.overlay.recording")}
          </span>
          <div className="sbase-r">
            <span className="stimer">
              {fmtTime(payload.elapsed_ms + Math.max(0, now - receivedAt))}
            </span>
            {cancelBtn}
          </div>
        </div>
      );
      break;
    case "held":
      cardClass = "level-warning";
      row = (
        <div className="sbase">
          <div className="sbase-l">
            <span className="tower-pause" aria-hidden="true" />
          </div>
          <span className="swork-label">{t("tower.overlay.held")}</span>
          <div className="sbase-r">{cancelBtn}</div>
        </div>
      );
      break;
    case "finishing":
      row = (
        <div className="sbase">
          <div className="sbase-l">
            <span className="sspinner" />
          </div>
          <span className="swork-label">{t("tower.overlay.finishing")}</span>
          <div className="sbase-r">{cancelBtn}</div>
        </div>
      );
      break;
    case "message":
      cardClass = `cnotice level-${payload.error ? "error" : "warning"}`;
      row = (
        <div className="sbase snotice">
          <NoticeBadgeIcon className="nglyph" />
          <span className="nlabel">
            {t(`tower.overlay.messages.${payload.message ?? "server_error"}`)}
          </span>
        </div>
      );
      break;
  }

  return (
    <div
      dir={direction}
      className={`ov-stage ${position} ov-fade ${isVisible ? "show" : ""}`}
    >
      <div className={`scard compact ctower ${cardClass}`}>{row}</div>
    </div>
  );
};
