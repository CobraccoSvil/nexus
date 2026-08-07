"use client";
import { useI18n } from "../../lib/i18n";

/**
 * Banner sticky di stato connessione SSE. Mostra "Connessione persa /
 * Riconnessione in corso" mentre `isReconnecting` e' true, e un toast verde
 * "Connessione ripristinata" quando il flag `reconnectSuccess` e' attivo (P6).
 * Reso in cima alla lista messaggi cosi' resta visibile durante lo scroll.
 */
export function ConnectionStatusBanner({
  isReconnecting,
  reconnectSuccess,
  tc,
}: {
  isReconnecting: boolean;
  reconnectSuccess: boolean;
  tc: Record<string, string>;
}) {
  const { t } = useI18n();
  return (
    <>
      {isReconnecting && (
        <div
          className="flex-row-gap-8 text-base"
          style={{
            position: "sticky",
            top: 0,
            zIndex: 8,
            alignSelf: "stretch",
            padding: "8px 12px",
            borderRadius: 10,
            border: "1px solid #f9731680",
            background: tc.bgCard,
            borderLeft: "3px solid #f97316",
            color: "#f97316",
          }}
        >
          <span style={{ animation: "spin 1s linear infinite", fontSize: 16 }}>↻</span>
          <strong>{t("chat.connessionePersa")}</strong>
          <span style={{ color: tc.textMuted, fontSize: 12 }}>
            — Riconnessione al server in corso, attendere…
          </span>
        </div>
      )}
      {reconnectSuccess && !isReconnecting && (
        <div
          className="flex-row-gap-8 text-base"
          style={{
            position: "sticky",
            top: 0,
            zIndex: 8,
            alignSelf: "stretch",
            padding: "8px 12px",
            borderRadius: 10,
            border: "1px solid #22c55e80",
            background: tc.bgCard,
            borderLeft: "3px solid #22c55e",
            color: "#22c55e",
          }}
        >
          <span style={{ fontSize: 16 }}>✓</span>
          <strong>{t("chat.connessioneRipristinata")}</strong>
        </div>
      )}
    </>
  );
}
