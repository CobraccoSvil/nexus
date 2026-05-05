"use client";

import { useState } from "react";
import { reloadGatewayConfig } from "../../lib/api-client";

interface GatewayConfigProps {
  items: Array<{ key: string; value: string; description: string; is_secret: boolean }>;
  onSaveComplete: () => void;
  /** Callback per aggiornare lo stato provider nel componente padre dopo il reload. */
  onRefreshProviders?: () => void;
}

export function GatewayConfig({ items, onSaveComplete, onRefreshProviders }: GatewayConfigProps) {
  const [reloading, setReloading] = useState(false);
  const [reloadMsg, setReloadMsg] = useState<string | null>(null);

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

  const card: React.CSSProperties = {
    background: "var(--color-bgCard)", border: "1px solid var(--color-border)",
    borderRadius: 8, padding: "16px 20px", marginBottom: 16,
  };
  const lbl: React.CSSProperties = {
    fontSize: 11, color: "var(--color-textMuted)", textTransform: "uppercase",
    letterSpacing: "0.08em", marginBottom: 10, fontWeight: 600,
  };

  return (
    <div style={{ fontFamily: "'JetBrains Mono', monospace", color: "var(--color-text)" }}>
      <div style={card}>
        <div style={lbl}>Hot-Reload Configurazione</div>
        <p style={{ fontSize: 12, color: "var(--color-textMuted)", marginBottom: 12 }}>
          Ricarica le API key e i flag enabled/disabled dal DB senza riavviare il gateway.
          I provider disabilitati dall&apos;admin vengono rimossi immediatamente.
          Lo stato aggiornato compare nel banner in cima alla pagina <strong>Provider LLM</strong>.
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
        <div style={lbl}>Impostazioni Gateway</div>
        <p style={{ fontSize: 12, color: "var(--color-textMuted)", marginBottom: 12 }}>
          Le API key e i flag provider si gestiscono in <strong>Providers</strong>.
          I parametri di routing in <strong>Routing</strong>.
          Dopo ogni modifica, clicca <em>Ricarica dal DB</em> per applicarle senza restart.
        </p>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
          {["rate_limit_per_tenant_requests", "rate_limit_per_provider_requests",
            "health_check_interval_ms", "default_max_tokens"].map((key) => {
            const item = items.find((i) => i.key === key);
            if (!item) return null;
            return (
              <div key={key} style={{
                padding: "8px 12px", background: "var(--color-bgInput)",
                borderRadius: 6, border: "1px solid var(--color-border)",
              }}>
                <div style={{ fontSize: 10, color: "var(--color-textMuted)", marginBottom: 2 }}>{key}</div>
                <div style={{ fontSize: 13, fontWeight: 600 }}>{item.value || "—"}</div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
