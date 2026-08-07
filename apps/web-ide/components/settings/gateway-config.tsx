"use client";

import { useState } from "react";
import { reloadGatewayConfig, updateAdminSetting } from "../../lib/api-client";
import { useI18n } from "../../lib/i18n";

interface GatewayConfigProps {
  items: Array<{ key: string; value: string; description: string; is_secret: boolean }>;
  onSaveComplete: () => void;
  /** Callback per aggiornare lo stato provider nel componente padre dopo il reload. */
  onRefreshProviders?: () => void;
}

export function GatewayConfig({ items, onSaveComplete, onRefreshProviders }: GatewayConfigProps) {
  const { t } = useI18n();
  const [reloading, setReloading] = useState(false);
  const [reloadMsg, setReloadMsg] = useState<string | null>(null);
  const [edit, setEdit] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState<Record<string, boolean>>({});
  const [saveMsg, setSaveMsg] = useState<string | null>(null);

  const handleReload = async () => {
    setReloading(true);
    setReloadMsg(null);
    try {
      const result = await reloadGatewayConfig();
      const r = result as { providers?: string[] };
      const provs = Array.isArray(r?.providers) ? r.providers! : [];
      setReloadMsg(`Ricaricato. Provider attivi: ${provs.join(", ") || "nessuno"}`);
      onRefreshProviders?.();
      onSaveComplete();
    } catch (e: unknown) {
      setReloadMsg(`Errore: ${(e as Error).message}`);
    } finally {
      setReloading(false);
    }
  };

  const handleSave = async (key: string) => {
    const nextValue = (edit[key] ?? "").trim();
    if (!nextValue) return;
    setSaveMsg(null);
    setSaving((prev) => ({ ...prev, [key]: true }));
    try {
      await updateAdminSetting(key, nextValue);
      setSaveMsg(`Salvato ${key}. Ora clicca "Ricarica dal DB" per applicare al gateway.`);
      onSaveComplete();
    } catch (e: unknown) {
      setSaveMsg(`Errore: ${(e as Error).message}`);
    } finally {
      setSaving((prev) => ({ ...prev, [key]: false }));
    }
  };

  const card: React.CSSProperties = {
    background: "var(--color-bgCard)", border: "1px solid var(--color-border)",
    borderRadius: 8, padding: "16px 20px", marginBottom: 16,
  };
  const lbl: React.CSSProperties = {
    fontSize: 11, color: "var(--color-textMuted)", textTransform: "uppercase",
    letterSpacing: "0.08em", marginBottom: 10, fontWeight: 600,
  };

  return (
    <div style={{ fontFamily: "var(--font-mono)", color: "var(--color-text)" }}>
      <div style={card}>
        <div style={lbl}>{t("settings.hotReloadConfigurazione")}</div>
        <p style={{ fontSize: 12, color: "var(--color-textMuted)", marginBottom: 12 }}>
          Ricarica le API key e i flag enabled/disabled dal DB senza riavviare il gateway.
          I provider disabilitati dall&apos;admin vengono rimossi immediatamente.
          Lo stato aggiornato compare nel banner in cima alla pagina <strong>{t("settings.providerLlm")}</strong>.
        </p>
        <button
          onClick={() => void handleReload()}
          disabled={reloading}
          style={{
            padding: "7px 18px", borderRadius: 6,
            background: reloading ? "var(--color-bgInput)" : "var(--color-accent)",
            color: reloading ? "var(--color-textMuted)" : "#fff",
            border: "none", cursor: reloading ? "not-allowed" : "pointer",
            fontSize: 12, fontWeight: 600,
          }}
        >
          {reloading ? "Ricaricamento..." : "Ricarica dal DB"}
        </button>
        {reloadMsg && (
          <p style={{ marginTop: 10, fontSize: 12,
            color: reloadMsg.startsWith("Errore") ? "#f87171" : "#4ade80" }}>
            {reloadMsg}
          </p>
        )}
      </div>

      <div style={card}>
        <div style={lbl}>{t("settings.impostazioniGateway")}</div>
        <p style={{ fontSize: 12, color: "var(--color-textMuted)", marginBottom: 12 }}>
          {t("settings.leApiKeyE")} <strong>{t("settings.providers")}</strong>.
          I parametri di routing in <strong>{t("settings.routing")}</strong>.
          Dopo ogni modifica, clicca <em>{t("settings.ricaricaDalDb")}</em> per applicarle senza restart.
        </p>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
          {["rate_limit_per_tenant_requests", "rate_limit_per_provider_requests",
            "health_check_interval_ms", "default_max_tokens"].map((key) => {
            const item = items.find((i) => i.key === key);
            if (!item) return null;
            const value = edit[key] ?? item.value ?? "";
            const isBusy = saving[key] ?? false;
            return (
              <div key={key} style={{
                padding: "8px 12px", background: "var(--color-bgInput)",
                borderRadius: 6, border: "1px solid var(--color-border)",
              }}>
                <div style={{ fontSize: 10, color: "var(--color-textMuted)", marginBottom: 2 }}>{key}</div>
                <div style={{ display: "flex", gap: 8, alignItems: "center", marginTop: 4, flexWrap: "wrap" }}>
                  <input
                    inputMode="numeric"
                    value={value}
                    onChange={(event) => setEdit((prev) => ({ ...prev, [key]: event.target.value }))}
                    placeholder={item.value || "—"}
                    style={{
                      flex: "1 1 140px",
                      fontFamily: "inherit",
                      fontSize: 13,
                      fontWeight: 600,
                      padding: "6px 8px",
                      borderRadius: 6,
                      border: "1px solid var(--color-border)",
                      background: "var(--color-bgCard)",
                      color: "var(--color-text)",
                    }}
                  />
                  <button
                    type="button"
                    onClick={() => void handleSave(key)}
                    disabled={isBusy || !value.trim() || value.trim() === (item.value ?? "").trim()}
                    style={{
                      padding: "6px 10px",
                      borderRadius: 6,
                      border: "1px solid var(--color-border)",
                      background: isBusy ? "var(--color-bgInput)" : "var(--color-bgCard)",
                      color: "var(--color-text)",
                      cursor: isBusy ? "not-allowed" : "pointer",
                      fontSize: 12,
                      fontWeight: 700,
                      opacity:
                        isBusy || !value.trim() || value.trim() === (item.value ?? "").trim()
                          ? 0.65
                          : 1,
                    }}
                    title={t("settings.salvaNelDbPoi")}
                  >
                    {isBusy ? "Salvo..." : "Salva"}
                  </button>
                </div>
                {item.description && (
                  <div style={{ fontSize: 11, color: "var(--color-textMuted)", marginTop: 6 }}>
                    {item.description}
                  </div>
                )}
              </div>
            );
          })}
        </div>
        {saveMsg && (
          <p
            style={{
              marginTop: 10,
              fontSize: 12,
              color: saveMsg.startsWith("Errore") ? "#f87171" : "#4ade80",
              whiteSpace: "pre-wrap",
            }}
          >
            {saveMsg}
          </p>
        )}
      </div>
    </div>
  );
}
