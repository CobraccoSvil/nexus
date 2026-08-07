"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useI18n } from "../../lib/i18n";

/**
 * Budget mensile per provider.
 *
 * I provider AI consumer (anthropic, openai, google, mistral) NON espongono
 * un endpoint pubblico per il balance via API key (verificato 2026-05-20).
 * DeepSeek e' l'unica eccezione (`GET /user/balance`) e viene sincronizzato
 * automaticamente da `deepseek_balance_sync.rs` ogni 15 min.
 *
 * Per gli altri, lo `spent_current_period_usd` viene incrementato in
 * `chat_messages.rs` quando un run completa. L'admin imposta `monthly_budget_usd`
 * quando ricarica l'account, e clicca "Ricarica" quando ha ricaricato davvero.
 *
 * Il `provider_health_probe` (Rust) marca un provider come unhealthy con
 * `error_kind=budget_exhausted` quando `(budget - spent) < min_threshold`.
 *
 * Questo modulo espone un hook (`useProviderBudgets`) e una riga presentazionale
 * (`ProviderBudgetRow`) riusati sia dal pannello standalone che dalle card
 * per-provider (providers-overview), cosi' il fetch e la logica sono un punto
 * unico (regola L).
 */

const API_BASE = process.env.NEXT_PUBLIC_API_URL || "";

export type BudgetEntry = {
  provider: string;
  monthly_budget_usd: string;
  spent_usd: string;
  remaining_usd: string;
  min_threshold_usd: string;
  is_exhausted: boolean;
  period_start: string;
  /** false = provider attivo (registry/catalog/key) ma senza riga budget in DB:
   *  "Imposta budget" la crea (UPSERT); "Ricarica" (UPDATE-only) resta disabilitato. */
  configured?: boolean;
};

type BudgetDraft = { budget: string; threshold: string };

export interface UseProviderBudgets {
  items: BudgetEntry[];
  byProvider: Record<string, BudgetEntry>;
  editing: Record<string, BudgetDraft>;
  setEditing: React.Dispatch<React.SetStateAction<Record<string, BudgetDraft>>>;
  busy: Record<string, boolean>;
  error: string | null;
  setBudget: (provider: string) => Promise<void>;
  recharge: (provider: string) => Promise<void>;
  reload: () => Promise<void>;
}

/** Hook: carica e gestisce i budget provider (fetch unico, azioni UPSERT/UPDATE). */
export function useProviderBudgets(): UseProviderBudgets {
  const [items, setItems] = useState<BudgetEntry[]>([]);
  const [editing, setEditing] = useState<Record<string, BudgetDraft>>({});
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      const r = await fetch(`${API_BASE}/api/admin/providers/budget`, { credentials: "include" });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const data = (await r.json()) as { providers: BudgetEntry[] };
      setItems(data.providers ?? []);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : "load fallito");
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const setBudget = useCallback(async (provider: string) => {
    const draft = editingRef.current[provider];
    if (!draft) return;
    setBusy((b) => ({ ...b, [provider]: true }));
    try {
      const r = await fetch(`${API_BASE}/api/admin/providers/${provider}/set-budget`, {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          monthly_budget_usd: parseFloat(draft.budget),
          min_threshold_usd: parseFloat(draft.threshold),
        }),
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      setEditing((e) => {
        const { [provider]: _removed, ...rest } = e;
        return rest;
      });
      await reload();
    } catch (e) {
      setError(e instanceof Error ? e.message : "set-budget fallito");
    } finally {
      setBusy((b) => ({ ...b, [provider]: false }));
    }
  }, [reload]);

  const recharge = useCallback(async (provider: string) => {
    setBusy((b) => ({ ...b, [provider]: true }));
    try {
      const r = await fetch(`${API_BASE}/api/admin/providers/${provider}/recharge-budget`, {
        method: "POST",
        credentials: "include",
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      await reload();
    } catch (e) {
      setError(e instanceof Error ? e.message : "recharge fallito");
    } finally {
      setBusy((b) => ({ ...b, [provider]: false }));
    }
  }, [reload]);

  // Ref agli editing correnti per evitare stale closure in setBudget.
  const editingRef = useRef(editing);
  editingRef.current = editing;

  const byProvider: Record<string, BudgetEntry> = {};
  for (const it of items) byProvider[it.provider] = it;

  return { items, byProvider, editing, setEditing, busy, error, setBudget, recharge, reload };
}

interface ProviderBudgetRowProps {
  entry: BudgetEntry;
  editing?: BudgetDraft;
  busy: boolean;
  setEditing: React.Dispatch<React.SetStateAction<Record<string, BudgetDraft>>>;
  onSetBudget: (provider: string) => void;
  onRecharge: (provider: string) => void;
  /** compact = riga snella per la card provider (nasconde il nome, gia' nell'header). */
  compact?: boolean;
}

/** Riga budget di un singolo provider (barra + Imposta/Ricarica). Presentazionale. */
export function ProviderBudgetRow({
  entry: it,
  editing: draft,
  busy,
  setEditing,
  onSetBudget,
  onRecharge,
  compact,
}: ProviderBudgetRowProps) {
  const { t } = useI18n();
  const budget = parseFloat(it.monthly_budget_usd);
  const spent = parseFloat(it.spent_usd);
  const remaining = parseFloat(it.remaining_usd);
  // Un budget impostato (>0) e' il presupposto di "esaurito": senza budget
  // (provider nuovo o non configurato) non c'e' esaurimento (allineato al
  // provider_health_probe, che considera esausti solo i provider con budget>0).
  const hasBudget = budget > 0;
  const configured = it.configured !== false && hasBudget;
  const pct = hasBudget ? Math.min(100, (spent / budget) * 100) : 0;
  const exhausted = configured && it.is_exhausted;

  return (
    <div style={{ padding: compact ? "8px 0 0" : 12, border: compact ? "none" : "1px solid var(--color-border)", borderRadius: 6, background: compact ? "transparent" : "var(--color-bgInput)", opacity: hasBudget ? 1 : 0.85 }}>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8, gap: 8, flexWrap: "wrap" }}>
        <div style={{ fontWeight: 600, fontSize: compact ? 12 : 14 }}>
          {compact ? "Budget mensile" : it.provider}
          {exhausted ? (
            <span style={{ marginLeft: 8, fontSize: 11, padding: "2px 8px", borderRadius: 4, background: "rgba(239,68,68,0.15)", color: "#c00" }}>
              {t("badge.esaurito")}
            </span>
          ) : !hasBudget ? (
            <span style={{ marginLeft: 8, fontSize: 11, padding: "2px 8px", borderRadius: 4, background: "var(--color-border)", color: "var(--color-textMuted)" }}>
              {t("badge.nonImpostato")}
            </span>
          ) : null}
        </div>
        <div style={{ fontSize: 12, color: "var(--color-textMuted)", fontFamily: "var(--font-mono)" }}>
          {hasBudget
            ? `$${remaining.toFixed(4)} / $${budget.toFixed(2)} (${(100 - pct).toFixed(1)}%)`
            : "budget non impostato"}
        </div>
      </div>
      <div style={{ height: 6, borderRadius: 3, background: "var(--color-border)", overflow: "hidden", marginBottom: 8 }}>
        <div style={{ height: "100%", width: `${pct}%`, background: pct > 90 ? "#c00" : pct > 70 ? "#f80" : "#0a0" }} />
      </div>
      <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", fontSize: 12 }}>
        {draft ? (
          <>
            <label>
              {t("settings.budget")} <input type="number" min="0" step="0.01" value={draft.budget} onChange={(e) => setEditing((s) => ({ ...s, [it.provider]: { ...draft, budget: e.target.value } }))} style={{ width: 80 }} />
            </label>
            <label>
              {t("settings.soglia")} <input type="number" min="0" step="0.01" value={draft.threshold} onChange={(e) => setEditing((s) => ({ ...s, [it.provider]: { ...draft, threshold: e.target.value } }))} style={{ width: 60 }} />
            </label>
            <button onClick={() => onSetBudget(it.provider)} disabled={busy} style={btnStyle("primary")}>
              {t("settings.salva")}
            </button>
            <button onClick={() => setEditing((s) => { const { [it.provider]: _removed, ...r } = s; return r; })} style={btnStyle("ghost")}>
              {t("settings.annulla")}
            </button>
          </>
        ) : (
          <>
            <button onClick={() => setEditing((s) => ({ ...s, [it.provider]: { budget: hasBudget ? it.monthly_budget_usd : "", threshold: it.min_threshold_usd } }))} style={btnStyle("ghost")}>
              {t("settings.impostaBudget")}
            </button>
            <button onClick={() => onRecharge(it.provider)} disabled={busy || !configured} style={btnStyle("ghost")} title={configured ? "Reset spent + period_start dopo ricarica reale" : "Imposta prima un budget"}>
              {t("settings.ricarica")}
            </button>
            <span style={{ color: "var(--color-textMuted)" }}>
              {configured
                ? `periodo dal ${new Date(it.period_start).toLocaleDateString("it")} · soglia $${parseFloat(it.min_threshold_usd).toFixed(2)}`
                : "nessun budget impostato per questo provider"}
            </span>
          </>
        )}
      </div>
    </div>
  );
}

/** Pannello standalone "Budget mensile provider" (usato fuori dalle card). */
export function ProviderBudget() {
  const { t } = useI18n();
  const { items, editing, setEditing, busy, error, setBudget, recharge } = useProviderBudgets();

  return (
    <div style={{ marginTop: 40, borderTop: "1px solid var(--color-border)", paddingTop: 24 }}>
      <h2 style={{ fontSize: 18, fontWeight: 600, marginBottom: 6 }}>{t("settings.budgetMensileProvider")}</h2>
      <p style={{ fontSize: 13, color: "var(--color-textMuted)", marginBottom: 20 }}>
        I provider AI consumer non espongono balance via API. Imposta tu il budget mensile (USD)
        quando ricarichi l&apos;account: ogni chiamata viene sottratta. Quando il residuo scende sotto
        la soglia minima, il provider diventa <b>unhealthy</b> e il routing dinamico lo evita.
        DeepSeek e&apos; sincronizzato automaticamente con il provider ogni 15 min.
      </p>

      {error && (
        <div style={{ marginBottom: 12, padding: 8, borderRadius: 4, background: "rgba(239,68,68,0.1)", color: "#c00", fontSize: 12 }}>
          {error}
        </div>
      )}

      <div style={{ display: "grid", gap: 12 }}>
        {items.map((it) => (
          <ProviderBudgetRow
            key={it.provider}
            entry={it}
            editing={editing[it.provider]}
            busy={!!busy[it.provider]}
            setEditing={setEditing}
            onSetBudget={setBudget}
            onRecharge={recharge}
          />
        ))}
      </div>
    </div>
  );
}

function btnStyle(kind: "primary" | "ghost"): React.CSSProperties {
  return {
    padding: "4px 12px",
    borderRadius: 4,
    border: "1px solid var(--color-border)",
    background: kind === "primary" ? "var(--color-accent, #4a9eff)" : "var(--color-bgInput)",
    color: kind === "primary" ? "#fff" : "var(--color-text)",
    cursor: "pointer",
    fontFamily: "inherit",
    fontSize: 12,
  };
}
