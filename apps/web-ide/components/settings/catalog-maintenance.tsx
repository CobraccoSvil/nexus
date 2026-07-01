"use client";

import { useState } from "react";

/**
 * Sezione "Catalogo modelli" nella pagina admin Provider LLM.
 *
 * Mostra due azioni di manutenzione che sono altrimenti accessibili solo via
 * REST diretto (POST /api/admin/sync-model-catalog e POST /api/admin/probe-models):
 *   - Sincronizza catalogo: scarica l'ultimo JSON LiteLLM da GitHub e fa
 *     upsert di prezzi/capabilities/context_window in ai_price_catalog. Non
 *     disabilita modelli orfani — quello lo fa il probe.
 *   - Verifica health modelli: forza un round one-shot di model_health_probe.
 *     Pinga ogni modello enabled, conta i fallimenti consecutivi, e disabilita
 *     automaticamente quelli sopra soglia (settings.model_health_probe_failure_threshold).
 *
 * Entrambe le azioni girano periodicamente in background (worker registrati
 * in mcp-core/src/main.rs). I bottoni servono per trigger manuale + feedback
 * immediato visibile all'admin.
 */

const API_BASE = process.env.NEXT_PUBLIC_API_URL || "";

type SyncResult = {
  added?: number;
  updated?: number;
  skipped?: number;
  source?: string;
  error?: string;
};

type ProbeResult = {
  ok?: boolean;
  total?: number;
  healthy?: number;
  provider_wide_errors?: number;
  model_errors?: number;
  auto_disabled?: number;
  skipped_provider_cooldown?: number;
  failure_threshold?: number;
  error?: string;
};

export function CatalogMaintenance() {
  const [syncing, setSyncing] = useState(false);
  const [probing, setProbing] = useState(false);
  const [syncResult, setSyncResult] = useState<SyncResult | null>(null);
  const [probeResult, setProbeResult] = useState<ProbeResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleSync = async () => {
    setSyncing(true);
    setError(null);
    setSyncResult(null);
    try {
      const res = await fetch(`${API_BASE}/api/admin/sync-model-catalog`, {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json" },
      });
      const data = (await res.json()) as SyncResult;
      if (!res.ok || data.error) {
        setError(data.error || `HTTP ${res.status}`);
      } else {
        setSyncResult(data);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Sync fallita");
    } finally {
      setSyncing(false);
    }
  };

  const handleProbe = async () => {
    setProbing(true);
    setError(null);
    setProbeResult(null);
    try {
      const res = await fetch(`${API_BASE}/api/admin/probe-models`, {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json" },
      });
      const data = (await res.json()) as ProbeResult;
      if (!res.ok || data.error) {
        setError(data.error || `HTTP ${res.status}`);
      } else {
        setProbeResult(data);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Probe fallito");
    } finally {
      setProbing(false);
    }
  };

  const buttonStyle: React.CSSProperties = {
    padding: "8px 16px",
    borderRadius: 6,
    border: "1px solid var(--color-border)",
    background: "var(--color-bgInput)",
    color: "var(--color-text)",
    cursor: "pointer",
    fontFamily: "inherit",
    fontSize: 13,
    fontWeight: 500,
  };

  return (
    <div style={{ marginTop: 40, borderTop: "1px solid var(--color-border)", paddingTop: 24 }}>
      <h2 style={{ fontSize: 18, fontWeight: 600, marginBottom: 6 }}>Catalogo modelli</h2>
      <p style={{ fontSize: 13, color: "var(--color-textMuted)", marginBottom: 20 }}>
        Sincronizzazione catalogo da LiteLLM (prezzi, context window, tool support) e verifica salute
        di ogni modello enabled. Entrambe le azioni girano automaticamente in background
        (configurabile in <code>settings</code>), questi bottoni servono per trigger manuale.
      </p>

      <div style={{ display: "flex", gap: 12, flexWrap: "wrap" }}>
        <button onClick={handleSync} disabled={syncing} style={buttonStyle} title="Scarica ultimo JSON LiteLLM e fa upsert in ai_price_catalog">
          {syncing ? "⏳ Sincronizzazione…" : "🔄 Sincronizza catalogo"}
        </button>
        <button onClick={handleProbe} disabled={probing} style={buttonStyle} title="Pinga ogni modello enabled, auto-disabilita quelli broken">
          {probing ? "⏳ Verifica in corso…" : "🩺 Verifica health modelli"}
        </button>
      </div>

      {error && (
        <div style={{ marginTop: 12, padding: 8, borderRadius: 4, background: "var(--color-bgError, #fee)", color: "var(--color-textError, #c00)", fontSize: 12 }}>
          Errore: {error}
        </div>
      )}

      {syncResult && (
        <div style={{ marginTop: 12, padding: 8, borderRadius: 4, background: "var(--color-bgInput)", fontSize: 12, fontFamily: "var(--font-mono)" }}>
          Sync OK ({syncResult.source}): aggiunti <b>{syncResult.added ?? 0}</b>, aggiornati{" "}
          <b>{syncResult.updated ?? 0}</b>, ignorati <b>{syncResult.skipped ?? 0}</b>.
        </div>
      )}

      {probeResult && (
        <div style={{ marginTop: 12, padding: 8, borderRadius: 4, background: "var(--color-bgInput)", fontSize: 12, fontFamily: "var(--font-mono)" }}>
          Probe completato su <b>{probeResult.total ?? 0}</b> modelli (soglia auto-disable:{" "}
          {probeResult.failure_threshold ?? "?"}): sani <b>{probeResult.healthy ?? 0}</b>, errori provider{" "}
          <b>{probeResult.provider_wide_errors ?? 0}</b>, errori modello{" "}
          <b>{probeResult.model_errors ?? 0}</b>, auto-disabilitati in questo round{" "}
          <b>{probeResult.auto_disabled ?? 0}</b>, saltati per cooldown provider{" "}
          <b>{probeResult.skipped_provider_cooldown ?? 0}</b>.
        </div>
      )}
    </div>
  );
}
