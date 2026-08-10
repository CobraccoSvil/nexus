"use client";

import { useEffect, useState } from "react";
import type { Route } from "next";
import Link from "next/link";
import { useThemeColors } from "../../lib/theme";
import { AdminPageHeader } from "../../components/admin/AdminPageHeader";
import { getGatewayProviders, reloadGatewayConfig } from "../../lib/api-client";
import { useProviderBudgets } from "../../components/settings/provider-budget";
import { renderDeclaration, renderReadiness, type GatewayProvider } from "../../lib/api/gateway-providers";

const SHORTCUTS: Array<{ label: string; href: Route; desc: string }> = [
  { label: "Provider & Modelli", href: "/admin/settings/providers" as Route, desc: "API key, modelli attivi, stato, budget" },
  { label: "Routing & Comportamento", href: "/admin/settings/routing" as Route, desc: "Modalita' Nexus, gerarchia provider, purpose" },
  { label: "Fatturazione", href: "/admin/billing" as Route, desc: "Prezzi, quote, consumo" },
  { label: "Utenti", href: "/admin/users" as Route, desc: "Ruoli e accessi" },
  { label: "Direttive prompt", href: "/admin/prompts/directives" as Route, desc: "Istruzioni condivise iniettate nei prompt" },
  { label: "Knowledge Base", href: "/admin/kb" as Route, desc: "Wiki e documentazione Nexus" },
];

export default function AdminDashboardPage() {
  const tc = useThemeColors();
  const [providers, setProviders] = useState<GatewayProvider[]>([]);
  const [reloadBusy, setReloadBusy] = useState(false);
  const [reloadMsg, setReloadMsg] = useState<string | null>(null);
  const budgets = useProviderBudgets();

  useEffect(() => {
    let active = true;
    getGatewayProviders()
      .then((d) => {
        if (!active) return;
        const list = (d as { providers?: GatewayProvider[] }).providers;
        setProviders(Array.isArray(list) ? list : []);
      })
      .catch(() => {});
    return () => { active = false; };
  }, []);

  async function handleReload() {
    setReloadBusy(true);
    setReloadMsg(null);
    try {
      await reloadGatewayConfig();
      setReloadMsg("Gateway ricaricato dal DB.");
      const d = await getGatewayProviders();
      const list = (d as { providers?: GatewayProvider[] }).providers;
      setProviders(Array.isArray(list) ? list : []);
    } catch (e) {
      setReloadMsg(`Errore: ${e instanceof Error ? e.message : "reload fallito"}`);
    } finally {
      setReloadBusy(false);
    }
  }

  const cardStyle: React.CSSProperties = {
    border: `1px solid ${tc.border}`,
    borderRadius: 10,
    padding: 16,
    background: tc.bgCard,
  };
  const cardTitle: React.CSSProperties = { fontSize: 13, fontWeight: 700, color: tc.text, marginBottom: 12, textTransform: "uppercase", letterSpacing: "0.05em" };

  const budgetedProviders = budgets.items.filter((b) => parseFloat(b.monthly_budget_usd) > 0);

  return (
    <div>
      <AdminPageHeader
        title="Panoramica Nexus"
        description="Stato dei provider, budget e scorciatoie all'uso quotidiano. Le configurazioni profonde restano sotto Configurazione avanzata."
        action={
          <button
            onClick={() => void handleReload()}
            disabled={reloadBusy}
            style={{
              padding: "6px 14px",
              borderRadius: 6,
              border: `1px solid ${tc.border}`,
              background: reloadBusy ? tc.bgInput : tc.bgCard,
              color: tc.text,
              cursor: reloadBusy ? "wait" : "pointer",
              fontFamily: "inherit",
              fontSize: 13,
            }}
          >
            {reloadBusy ? "..." : "Ricarica gateway"}
          </button>
        }
      />

      {reloadMsg && (
        <div style={{ fontSize: 12, color: reloadMsg.startsWith("Errore") ? tc.error : tc.success, marginBottom: 12 }}>
          {reloadMsg}
        </div>
      )}

      <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(300px, 1fr))", gap: 16 }}>
        {/* ── Stato provider ── */}
        <div style={cardStyle}>
          <div style={cardTitle}>Provider</div>
          {providers.length === 0 ? (
            <div style={{ fontSize: 12, color: tc.textMuted }}>Nessun dato provider dal gateway.</div>
          ) : (
            <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              {providers.map((p) => {
                // Due domande, due righe: la salute e la copertura della
                // dichiarazione non si possono fondere in un'etichetta sola —
                // groq e openrouter sono `attivo` E senza una riga di
                // capability, e una riga sola dovrebbe sceglierne una.
                const dichiarazione = renderDeclaration(p);
                return (
                  <div key={p.name} style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                    <div style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 13 }}>
                      <span style={{
                        width: 8, height: 8, borderRadius: "50%", flexShrink: 0,
                        background: p.healthy === true ? "#4ade80"
                          : p.healthy === false ? "#f87171"
                          : renderReadiness(p).requiresAction ? "#fbbf24" : "#9ca3af",
                      }} />
                      <span style={{ fontWeight: 600, color: tc.text }}>{p.name}</span>
                      <span style={{
                        marginLeft: "auto", fontSize: 11,
                        color: renderReadiness(p).requiresAction ? "#fbbf24" : tc.textMuted,
                      }}>
                        {renderReadiness(p).label}
                      </span>
                    </div>
                    {dichiarazione && (
                      <div style={{
                        fontSize: 11, paddingLeft: 16,
                        color: dichiarazione.requiresAction ? "#fbbf24" : tc.textMuted,
                      }}>
                        {dichiarazione.label}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </div>

        {/* ── Budget mensile ── */}
        <div style={cardStyle}>
          <div style={cardTitle}>Budget mensile</div>
          {budgetedProviders.length === 0 ? (
            <div style={{ fontSize: 12, color: tc.textMuted }}>
              Nessun budget impostato.{" "}
              <Link href={"/admin/settings/providers" as Route} style={{ color: tc.accent }}>Impostane uno</Link>.
            </div>
          ) : (
            <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
              {budgetedProviders.map((b) => {
                const budget = parseFloat(b.monthly_budget_usd);
                const spent = parseFloat(b.spent_usd);
                const pct = Math.min(100, (spent / budget) * 100);
                return (
                  <div key={b.provider}>
                    <div style={{ display: "flex", justifyContent: "space-between", fontSize: 12, marginBottom: 3 }}>
                      <span style={{ fontWeight: 600, color: tc.text }}>{b.provider}</span>
                      <span style={{ color: tc.textMuted, fontFamily: "var(--font-mono)" }}>
                        ${spent.toFixed(2)} / ${budget.toFixed(2)} ({pct.toFixed(0)}%)
                      </span>
                    </div>
                    <div style={{ height: 6, borderRadius: 3, background: tc.border, overflow: "hidden" }}>
                      <div style={{ height: "100%", width: `${pct}%`, background: pct > 90 ? "#c00" : pct > 70 ? "#f80" : "#0a0" }} />
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>

        {/* ── Scorciatoie uso quotidiano ── */}
        <div style={cardStyle}>
          <div style={cardTitle}>Impostazioni frequenti</div>
          <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            {SHORTCUTS.map((s) => (
              <Link
                key={s.href}
                href={s.href}
                style={{
                  display: "block",
                  padding: "8px 10px",
                  borderRadius: 6,
                  textDecoration: "none",
                  background: tc.bgInput,
                  border: `1px solid ${tc.border}`,
                }}
              >
                <div style={{ fontSize: 13, fontWeight: 600, color: tc.accent }}>{s.label}</div>
                <div style={{ fontSize: 11, color: tc.textMuted }}>{s.desc}</div>
              </Link>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
