"use client";

import { useCallback, useEffect, useState } from "react";

/**
 * Pannello "Budget mensile per provider".
 *
 * I provider AI consumer (anthropic, openai, google, mistral) NON espongono
 * un endpoint pubblico per il balance via API key (verificato 2026-05-20).
 * DeepSeek e' l'unica eccezione (`GET /user/balance`) e viene sincronizzato
 * automaticamente da `deepseek_balance_sync.rs` ogni 15 min.
 *
 * Per gli altri 4, lo `spent_current_period_usd` viene incrementato in
 * `chat_messages.rs` quando un run completa (`total_cost` × tokens). L'admin
 * imposta `monthly_budget_usd` quando ricarica l'account presso il provider,
 * e clicca "Ricarica" quando ha ricaricato per davvero (reset spent + period_start).
 *
 * Il `provider_health_probe` (Rust) marca un provider come unhealthy con
 * `error_kind=budget_exhausted` quando `(budget - spent) < min_threshold`.
 */

const API_BASE = process.env.NEXT_PUBLIC_API_URL || "";

type BudgetEntry = {
  provider: string;
  monthly_budget_usd: string;
  spent_usd: string;
  remaining_usd: string;
  min_threshold_usd: string;
  is_exhausted: boolean;
  period_start: string;
};

export function ProviderBudget() {
  const [items, setItems] = useState<BudgetEntry[]>([]);
  const [editing, setEditing] = useState<Record<string, { budget: string; threshold: string }>>({});
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
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
    void load();
  }, [load]);

  const setBudget = async (provider: string) => {
    const draft = editing[provider];
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
        const { [provider]: _, ...rest } = e;
        return rest;
      });
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "set-budget fallito");
    } finally {
      setBusy((b) => ({ ...b, [provider]: false }));
    }
  };

  const recharge = async (provider: string) => {
    if (!window.confirm) return; // safeguard, ma in pratica usiamo confirmDialog del parent se serve
    setBusy((b) => ({ ...b, [provider]: true }));
    try {
      const r = await fetch(`${API_BASE}/api/admin/providers/${provider}/recharge-budget`, {
        method: "POST",
        credentials: "include",
      });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "recharge fallito");
    } finally {
      setBusy((b) => ({ ...b, [provider]: false }));
    }
  };

  return (
    <div style={{ marginTop: 40, borderTop: "1px solid var(--color-border)", paddingTop: 24 }}>
      <h2 style={{ fontSize: 18, fontWeight: 600, marginBottom: 6 }}>Budget mensile provider</h2>
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
        {items.map((it) => {
          const budget = parseFloat(it.monthly_budget_usd);
          const spent = parseFloat(it.spent_usd);
          const remaining = parseFloat(it.remaining_usd);
          const pct = budget > 0 ? Math.min(100, (spent / budget) * 100) : 100;
          const draft = editing[it.provider];
          return (
            <div key={it.provider} style={{ padding: 12, border: "1px solid var(--color-border)", borderRadius: 6, background: "var(--color-bgInput)" }}>
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
                <div style={{ fontWeight: 600, fontSize: 14 }}>
                  {it.provider}
                  {it.is_exhausted && (
                    <span style={{ marginLeft: 8, fontSize: 11, padding: "2px 8px", borderRadius: 4, background: "rgba(239,68,68,0.15)", color: "#c00" }}>
                      ESAURITO
                    </span>
                  )}
                </div>
                <div style={{ fontSize: 12, color: "var(--color-textMuted)", fontFamily: "var(--font-mono)" }}>
                  ${remaining.toFixed(4)} / ${budget.toFixed(2)} ({(100 - pct).toFixed(1)}%)
                </div>
              </div>
              <div style={{ height: 6, borderRadius: 3, background: "var(--color-border)", overflow: "hidden", marginBottom: 8 }}>
                <div style={{ height: "100%", width: `${pct}%`, background: pct > 90 ? "#c00" : pct > 70 ? "#f80" : "#0a0" }} />
              </div>
              <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", fontSize: 12 }}>
                {draft ? (
                  <>
                    <label>
                      Budget $: <input type="number" min="0" step="0.01" value={draft.budget} onChange={(e) => setEditing((s) => ({ ...s, [it.provider]: { ...draft, budget: e.target.value } }))} style={{ width: 80 }} />
                    </label>
                    <label>
                      Soglia $: <input type="number" min="0" step="0.01" value={draft.threshold} onChange={(e) => setEditing((s) => ({ ...s, [it.provider]: { ...draft, threshold: e.target.value } }))} style={{ width: 60 }} />
                    </label>
                    <button onClick={() => setBudget(it.provider)} disabled={busy[it.provider]} style={btnStyle("primary")}>
                      Salva
                    </button>
                    <button onClick={() => setEditing((s) => { const { [it.provider]: _, ...r } = s; return r; })} style={btnStyle("ghost")}>
                      Annulla
                    </button>
                  </>
                ) : (
                  <>
                    <button onClick={() => setEditing((s) => ({ ...s, [it.provider]: { budget: it.monthly_budget_usd, threshold: it.min_threshold_usd } }))} style={btnStyle("ghost")}>
                      Imposta budget
                    </button>
                    <button onClick={() => recharge(it.provider)} disabled={busy[it.provider]} style={btnStyle("ghost")} title="Reset spent + period_start dopo ricarica reale">
                      Ricarica
                    </button>
                    <span style={{ color: "var(--color-textMuted)" }}>periodo dal {new Date(it.period_start).toLocaleDateString("it")} · soglia ${parseFloat(it.min_threshold_usd).toFixed(2)}</span>
                  </>
                )}
              </div>
            </div>
          );
        })}
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
