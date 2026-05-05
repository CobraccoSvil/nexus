"use client";

import { FormEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  AdminProjectSummary,
  AdminUser,
  BillingPrice,
  BillingQuota,
  BillingUsageReport,
  ModelCatalogItem,
  createBillingPrice,
  createBillingQuota,
  getAdminBillingUsage,
  listAdminProjects,
  listAdminUsers,
  listBillingPrices,
  listBillingQuotas,
  listModelCatalog,
} from "../../../lib/api-client";
import { useThemeColors } from "../../../lib/theme";

function toIsoDateStart(value: string): string | undefined {
  if (!value) return undefined;
  return `${value}T00:00:00Z`;
}

function toIsoDateEnd(value: string): string | undefined {
  if (!value) return undefined;
  return `${value}T23:59:59Z`;
}

type Tc = ReturnType<typeof useThemeColors>;

const KNOWN_PROVIDERS = ["anthropic", "openai", "google", "deepseek", "mistral"];

export default function AdminBillingPage() {
  const tc = useThemeColors();
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Reference data
  const [users, setUsers] = useState<AdminUser[]>([]);
  const [projects, setProjects] = useState<AdminProjectSummary[]>([]);
  const [catalog, setCatalog] = useState<ModelCatalogItem[]>([]);

  // Billing data
  const [prices, setPrices] = useState<BillingPrice[]>([]);
  const [quotas, setQuotas] = useState<BillingQuota[]>([]);
  const [usage, setUsage] = useState<BillingUsageReport | null>(null);

  // Filters
  const [dateFrom, setDateFrom] = useState("");
  const [dateTo, setDateTo] = useState("");
  const [filterUserId, setFilterUserId] = useState("");
  const [filterProjectId, setFilterProjectId] = useState("");
  const [filterProvider, setFilterProvider] = useState("");
  const [filterStatus, setFilterStatus] = useState("");

  const [busy, setBusy] = useState<"price" | "quota" | "refresh" | null>(null);

  // Form: nuovo prezzo (selezione dal catalogo)
  const [newPriceCatalogKey, setNewPriceCatalogKey] = useState<string>("");
  const [newPriceInput, setNewPriceInput] = useState<string>("");
  const [newPriceOutput, setNewPriceOutput] = useState<string>("");
  const [newPriceCurrency, setNewPriceCurrency] = useState<string>("EUR");

  // Form: nuova quota
  const [newQuota, setNewQuota] = useState({
    scope_type: "project" as "user" | "project" | "user_project",
    user_id: "",
    project_id: "",
    token_limit: "",
    cost_limit: "",
    currency: "EUR",
    valid_from: "",
    valid_to: "",
    note: "",
  });

  const providers = useMemo(() => {
    const set = new Set<string>(KNOWN_PROVIDERS);
    catalog.forEach((m) => set.add(m.provider));
    // Aggiungi anche i provider presenti nell'usage report corrente
    usage?.breakdown.forEach((b) => set.add(b.provider));
    return Array.from(set).sort();
  }, [catalog, usage]);

  // Ref ai filtri correnti per l'auto-refresh (evita stale closure nel listener)
  const dateFromRef = useRef(dateFrom);
  const dateToRef = useRef(dateTo);
  const filterUserIdRef = useRef(filterUserId);
  const filterProjectIdRef = useRef(filterProjectId);
  const filterProviderRef = useRef(filterProvider);
  const filterStatusRef = useRef(filterStatus);
  useEffect(() => { dateFromRef.current = dateFrom; }, [dateFrom]);
  useEffect(() => { dateToRef.current = dateTo; }, [dateTo]);
  useEffect(() => { filterUserIdRef.current = filterUserId; }, [filterUserId]);
  useEffect(() => { filterProjectIdRef.current = filterProjectId; }, [filterProjectId]);
  useEffect(() => { filterProviderRef.current = filterProvider; }, [filterProvider]);
  useEffect(() => { filterStatusRef.current = filterStatus; }, [filterStatus]);

  const loadReference = async () => {
    try {
      const [usersRes, projectsRes, catalogRes] = await Promise.all([
        listAdminUsers(1, 200).catch(() => ({ users: [] as AdminUser[] })),
        listAdminProjects().catch(() => ({ projects: [] as AdminProjectSummary[] })),
        listModelCatalog().catch(() => ({ models: [] as ModelCatalogItem[] })),
      ]);
      setUsers(usersRes.users || []);
      setProjects(projectsRes.projects || []);
      setCatalog(catalogRes.models || []);
    } catch (refError) {
      setError(refError instanceof Error ? refError.message : "Reference load failed");
    }
  };

  const loadBilling = useCallback(async (from?: string, to?: string) => {
    const resolvedFrom = from ?? dateFromRef.current;
    const resolvedTo = to ?? dateToRef.current;
    setBusy("refresh");
    setError(null);
    try {
      const [pricesRes, quotasRes, usageRes] = await Promise.all([
        listBillingPrices(),
        listBillingQuotas(),
        getAdminBillingUsage({
          date_from: toIsoDateStart(resolvedFrom),
          date_to: toIsoDateEnd(resolvedTo),
          user_id: filterUserIdRef.current || undefined,
          project_id: filterProjectIdRef.current || undefined,
          provider: filterProviderRef.current || undefined,
          status: filterStatusRef.current || undefined,
        }),
      ]);
      setPrices(pricesRes.prices || []);
      setQuotas(quotasRes.quotas || []);
      setUsage(usageRes);
    } catch (loadError) {
      setError(loadError instanceof Error ? loadError.message : "Load failed");
    } finally {
      setBusy(null);
      setLoading(false);
    }
  }, []);

  // Caricamento iniziale
  useEffect(() => {
    void (async () => {
      await loadReference();
      await loadBilling();
    })();
  }, [loadBilling]);

  // Auto-refresh quando la pagina torna visibile (cambio tab o navigazione admin)
  useEffect(() => {
    const handleVisibility = () => {
      if (document.visibilityState === "visible") {
        void loadBilling();
      }
    };
    document.addEventListener("visibilitychange", handleVisibility);
    return () => document.removeEventListener("visibilitychange", handleVisibility);
  }, [loadBilling]);

  // Quando l'utente sceglie un modello dal catalogo, prefilla i prezzi di riferimento
  useEffect(() => {
    if (!newPriceCatalogKey) {
      setNewPriceInput("");
      setNewPriceOutput("");
      return;
    }
    const item = catalog.find((m) => `${m.provider}::${m.model}` === newPriceCatalogKey);
    if (item) {
      setNewPriceInput(String(item.inputCostPerMillionTokens));
      setNewPriceOutput(String(item.outputCostPerMillionTokens));
      setNewPriceCurrency(item.currency || "EUR");
    }
  }, [newPriceCatalogKey, catalog]);

  const onCreatePrice = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!newPriceCatalogKey) {
      setError("Seleziona un provider e un modello dal catalogo");
      return;
    }
    const [provider, model] = newPriceCatalogKey.split("::");
    setBusy("price");
    setError(null);
    try {
      await createBillingPrice({
        provider,
        model,
        input_cost_per_million_tokens: Number(newPriceInput || "0"),
        output_cost_per_million_tokens: Number(newPriceOutput || "0"),
        currency: (newPriceCurrency || "EUR").trim().toUpperCase(),
      });
      await loadBilling();
    } catch (saveError) {
      setError(saveError instanceof Error ? saveError.message : "Create price failed");
    } finally {
      setBusy(null);
    }
  };

  const onCreateQuota = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    // Validazione: scope coerente
    if ((newQuota.scope_type === "user" || newQuota.scope_type === "user_project") && !newQuota.user_id) {
      setError("Seleziona un utente per la quota");
      return;
    }
    if ((newQuota.scope_type === "project" || newQuota.scope_type === "user_project") && !newQuota.project_id) {
      setError("Seleziona un progetto per la quota");
      return;
    }
    setBusy("quota");
    setError(null);
    try {
      await createBillingQuota({
        scope_type: newQuota.scope_type,
        user_id: newQuota.user_id || undefined,
        project_id: newQuota.project_id || undefined,
        token_limit: newQuota.token_limit.trim() ? Number(newQuota.token_limit) : undefined,
        cost_limit: newQuota.cost_limit.trim() ? Number(newQuota.cost_limit) : undefined,
        currency: newQuota.currency.trim().toUpperCase() || undefined,
        valid_from: toIsoDateStart(newQuota.valid_from) || new Date().toISOString(),
        valid_to:
          toIsoDateEnd(newQuota.valid_to) ||
          new Date(Date.now() + 30 * 24 * 60 * 60 * 1000).toISOString(),
        note: newQuota.note.trim() || undefined,
      });
      setNewQuota({
        scope_type: "project",
        user_id: "",
        project_id: "",
        token_limit: "",
        cost_limit: "",
        currency: "EUR",
        valid_from: "",
        valid_to: "",
        note: "",
      });
      await loadBilling();
    } catch (saveError) {
      setError(saveError instanceof Error ? saveError.message : "Create quota failed");
    } finally {
      setBusy(null);
    }
  };

  const userById = useMemo(() => {
    const m = new Map<string, AdminUser>();
    users.forEach((u) => m.set(u.id, u));
    return m;
  }, [users]);

  const projectById = useMemo(() => {
    const m = new Map<string, AdminProjectSummary>();
    projects.forEach((p) => m.set(p.id, p));
    return m;
  }, [projects]);

  if (loading) {
    return <div style={{ color: tc.textMuted, padding: 24 }}>Caricamento billing...</div>;
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 18, padding: 4 }}>
      <div>
        <h1 style={{ fontSize: 22, fontWeight: 600, marginBottom: 6, color: tc.text }}>Billing AI</h1>
        <p style={{ color: tc.textMuted, fontSize: 13, margin: 0 }}>
          Catalogo modelli, prezzi attivi, quote per utente/progetto e report consumi.
        </p>
      </div>

      {error && (
        <div
          style={{
            padding: "10px 14px",
            borderRadius: 8,
            border: `1px solid ${tc.error}`,
            color: tc.error,
            background: tc.bgCard,
            fontSize: 13,
          }}
        >
          {error}
        </div>
      )}

      {/* ─── Report utilizzo ─── */}
      <section style={cardStyle(tc)}>
        <div style={sectionHeader}>
          <strong style={{ fontSize: 14, color: tc.text }}>Report utilizzo</strong>
          <span style={{ fontSize: 11, color: tc.textMuted, marginLeft: 8 }}>
            Filtra per intervallo, utente, progetto o provider
          </span>
        </div>

        <div style={{ display: "grid", gap: 8, gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", marginBottom: 12 }}>
          <label style={labelStyle(tc)}>
            Da
            <input type="date" value={dateFrom} onChange={(e) => setDateFrom(e.target.value)} style={inputStyle(tc)} />
          </label>
          <label style={labelStyle(tc)}>
            A
            <input type="date" value={dateTo} onChange={(e) => setDateTo(e.target.value)} style={inputStyle(tc)} />
          </label>
          <label style={labelStyle(tc)}>
            Utente
            <select value={filterUserId} onChange={(e) => setFilterUserId(e.target.value)} style={inputStyle(tc)}>
              <option value="">Tutti</option>
              {users.map((u) => (
                <option key={u.id} value={u.id}>
                  {u.displayName || u.email} ({u.email})
                </option>
              ))}
            </select>
          </label>
          <label style={labelStyle(tc)}>
            Progetto
            <select value={filterProjectId} onChange={(e) => setFilterProjectId(e.target.value)} style={inputStyle(tc)}>
              <option value="">Tutti</option>
              {projects.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
            </select>
          </label>
          <label style={labelStyle(tc)}>
            Provider
            <select value={filterProvider} onChange={(e) => setFilterProvider(e.target.value)} style={inputStyle(tc)}>
              <option value="">Tutti</option>
              {providers.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </select>
          </label>
          <label style={labelStyle(tc)}>
            Stato
            <select value={filterStatus} onChange={(e) => setFilterStatus(e.target.value)} style={inputStyle(tc)}>
              <option value="">Tutti</option>
              <option value="finalized">Finalized</option>
              <option value="reserved">Reserved</option>
              <option value="released">Released</option>
            </select>
          </label>
          <div style={{ display: "flex", alignItems: "flex-end" }}>
            <button onClick={() => void loadBilling(dateFrom, dateTo)} disabled={busy === "refresh"} style={buttonStyle(tc, true)}>
              {busy === "refresh" ? "Aggiorno..." : "Aggiorna"}
            </button>
          </div>
        </div>

        <div style={{ display: "flex", gap: 16, flexWrap: "wrap", fontSize: 13, marginBottom: 14, color: tc.text }}>
          <div>Token totali: <strong>{(usage?.summary.total_tokens ?? 0).toLocaleString('it-IT')}</strong></div>
          <div>Costo totale: <strong>{(usage?.summary.total_cost ?? 0).toFixed(4)} EUR</strong></div>
          <div>Run: <strong>{usage?.summary.total_runs ?? 0}</strong></div>
        </div>

        {usage && usage.breakdown.length > 0 ? (
          <div style={{ overflow: "auto", border: `1px solid ${tc.border}`, borderRadius: 8 }}>
            <table style={tableStyle(tc)}>
              <thead>
                <tr>
                  <th style={thStyle(tc)}>Provider</th>
                  <th style={thStyle(tc)}>Modello</th>
                  <th style={thStyleR(tc)}>Token</th>
                  <th style={thStyleR(tc)}>Costo</th>
                  <th style={thStyleR(tc)}>Run</th>
                </tr>
              </thead>
              <tbody>
                {usage.breakdown.map((b, idx) => (
                  <tr key={`${b.provider}-${b.model}-${idx}`}>
                    <td style={tdStyle(tc)}>{b.provider}</td>
                    <td style={tdStyle(tc)}>{b.model}</td>
                    <td style={tdStyleR(tc)}>{b.total_tokens.toLocaleString('it-IT')}</td>
                    <td style={tdStyleR(tc)}>{b.total_cost.toFixed(4)}</td>
                    <td style={tdStyleR(tc)}>{b.runs}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <div style={{ fontSize: 12, color: tc.textMuted }}>Nessun consumo registrato per i filtri selezionati.</div>
        )}
      </section>

      {/* ─── Catalogo modelli ─── */}
      <section style={cardStyle(tc)}>
        <div style={sectionHeader}>
          <strong style={{ fontSize: 14, color: tc.text }}>Catalogo modelli ({catalog.length})</strong>
          <span style={{ fontSize: 11, color: tc.textMuted, marginLeft: 8 }}>
            Costi di riferimento per milione di token (USD)
          </span>
        </div>
        <div style={{ overflow: "auto", border: `1px solid ${tc.border}`, borderRadius: 8 }}>
          <table style={tableStyle(tc)}>
            <thead>
              <tr>
                <th style={thStyle(tc)}>Provider</th>
                <th style={thStyle(tc)}>Modello</th>
                <th style={thStyle(tc)}>Display</th>
                <th style={thStyle(tc)}>Tier</th>
                <th style={thStyleR(tc)}>Input / 1M</th>
                <th style={thStyleR(tc)}>Output / 1M</th>
                <th style={thStyleR(tc)}>Context</th>
              </tr>
            </thead>
            <tbody>
              {catalog.map((m) => (
                <tr key={`${m.provider}-${m.model}`}>
                  <td style={tdStyle(tc)}>{m.provider}</td>
                  <td style={{ ...tdStyle(tc), fontFamily: "monospace", fontSize: 11 }}>{m.model}</td>
                  <td style={tdStyle(tc)}>{m.displayName}</td>
                  <td style={tdStyle(tc)}>
                    <span style={tierBadge(tc, m.performanceTier)}>{m.performanceTier}</span>
                  </td>
                  <td style={tdStyleR(tc)}>${m.inputCostPerMillionTokens.toFixed(2)}</td>
                  <td style={tdStyleR(tc)}>${m.outputCostPerMillionTokens.toFixed(2)}</td>
                  <td style={tdStyleR(tc)}>{m.contextWindow.toLocaleString('it-IT')}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>

      {/* ─── Nuovo prezzo ─── */}
      <section style={cardStyle(tc)}>
        <div style={sectionHeader}>
          <strong style={{ fontSize: 14, color: tc.text }}>Nuovo prezzo (override per provider/modello)</strong>
          <span style={{ fontSize: 11, color: tc.textMuted, marginLeft: 8 }}>
            Selezionando un modello dal catalogo i prezzi vengono prefillati
          </span>
        </div>
        <form onSubmit={onCreatePrice} style={{ display: "grid", gap: 10, gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", alignItems: "end" }}>
          <label style={labelStyle(tc)}>
            Modello
            <select
              value={newPriceCatalogKey}
              onChange={(e) => setNewPriceCatalogKey(e.target.value)}
              style={inputStyle(tc)}
              required
            >
              <option value="">— Seleziona —</option>
              {providers.map((p) => (
                <optgroup key={p} label={p}>
                  {catalog.filter((m) => m.provider === p).map((m) => (
                    <option key={`${m.provider}::${m.model}`} value={`${m.provider}::${m.model}`}>
                      {m.displayName} ({m.model})
                    </option>
                  ))}
                </optgroup>
              ))}
            </select>
          </label>
          <label style={labelStyle(tc)}>
            Costo input / 1M
            <input
              type="number"
              step="0.0001"
              min="0"
              value={newPriceInput}
              onChange={(e) => setNewPriceInput(e.target.value)}
              style={inputStyle(tc)}
              required
            />
          </label>
          <label style={labelStyle(tc)}>
            Costo output / 1M
            <input
              type="number"
              step="0.0001"
              min="0"
              value={newPriceOutput}
              onChange={(e) => setNewPriceOutput(e.target.value)}
              style={inputStyle(tc)}
              required
            />
          </label>
          <label style={labelStyle(tc)}>
            Valuta
            <select value={newPriceCurrency} onChange={(e) => setNewPriceCurrency(e.target.value)} style={inputStyle(tc)}>
              <option value="EUR">EUR</option>
              <option value="USD">USD</option>
            </select>
          </label>
          <button type="submit" disabled={busy === "price"} style={buttonStyle(tc, true)}>
            {busy === "price" ? "Salvo..." : "Crea prezzo"}
          </button>
        </form>
        <div style={{ marginTop: 10, fontSize: 12, color: tc.textMuted }}>Prezzi configurati: {prices.length}</div>
      </section>

      {/* ─── Nuova quota ─── */}
      <section style={cardStyle(tc)}>
        <div style={sectionHeader}>
          <strong style={{ fontSize: 14, color: tc.text }}>Nuova quota</strong>
          <span style={{ fontSize: 11, color: tc.textMuted, marginLeft: 8 }}>
            Limita token o costo per utente/progetto su un intervallo temporale
          </span>
        </div>
        <form onSubmit={onCreateQuota} style={{ display: "grid", gap: 10, gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))", alignItems: "end" }}>
          <label style={labelStyle(tc)}>
            Scope
            <select
              value={newQuota.scope_type}
              onChange={(e) =>
                setNewQuota((v) => ({ ...v, scope_type: e.target.value as "user" | "project" | "user_project" }))
              }
              style={inputStyle(tc)}
            >
              <option value="user">Utente</option>
              <option value="project">Progetto</option>
              <option value="user_project">Utente + Progetto</option>
            </select>
          </label>

          {(newQuota.scope_type === "user" || newQuota.scope_type === "user_project") && (
            <label style={labelStyle(tc)}>
              Utente
              <select
                value={newQuota.user_id}
                onChange={(e) => setNewQuota((v) => ({ ...v, user_id: e.target.value }))}
                style={inputStyle(tc)}
                required
              >
                <option value="">— Seleziona —</option>
                {users.map((u) => (
                  <option key={u.id} value={u.id}>
                    {u.displayName || u.email} ({u.email})
                  </option>
                ))}
              </select>
            </label>
          )}

          {(newQuota.scope_type === "project" || newQuota.scope_type === "user_project") && (
            <label style={labelStyle(tc)}>
              Progetto
              <select
                value={newQuota.project_id}
                onChange={(e) => setNewQuota((v) => ({ ...v, project_id: e.target.value }))}
                style={inputStyle(tc)}
                required
              >
                <option value="">— Seleziona —</option>
                {projects.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </select>
            </label>
          )}

          <label style={labelStyle(tc)}>
            Limite token
            <input
              type="number"
              min="0"
              value={newQuota.token_limit}
              onChange={(e) => setNewQuota((v) => ({ ...v, token_limit: e.target.value }))}
              placeholder="es. 1000000"
              style={inputStyle(tc)}
            />
          </label>
          <label style={labelStyle(tc)}>
            Limite costo
            <input
              type="number"
              step="0.01"
              min="0"
              value={newQuota.cost_limit}
              onChange={(e) => setNewQuota((v) => ({ ...v, cost_limit: e.target.value }))}
              placeholder="es. 100"
              style={inputStyle(tc)}
            />
          </label>
          <label style={labelStyle(tc)}>
            Valuta
            <select value={newQuota.currency} onChange={(e) => setNewQuota((v) => ({ ...v, currency: e.target.value }))} style={inputStyle(tc)}>
              <option value="EUR">EUR</option>
              <option value="USD">USD</option>
            </select>
          </label>
          <label style={labelStyle(tc)}>
            Valido dal
            <input type="date" value={newQuota.valid_from} onChange={(e) => setNewQuota((v) => ({ ...v, valid_from: e.target.value }))} style={inputStyle(tc)} />
          </label>
          <label style={labelStyle(tc)}>
            Valido fino al
            <input type="date" value={newQuota.valid_to} onChange={(e) => setNewQuota((v) => ({ ...v, valid_to: e.target.value }))} style={inputStyle(tc)} />
          </label>
          <label style={{ ...labelStyle(tc), gridColumn: "1 / -1" }}>
            Note
            <input
              value={newQuota.note}
              onChange={(e) => setNewQuota((v) => ({ ...v, note: e.target.value }))}
              placeholder="Descrizione opzionale"
              style={inputStyle(tc)}
            />
          </label>
          <button type="submit" disabled={busy === "quota"} style={buttonStyle(tc, true)}>
            {busy === "quota" ? "Salvo..." : "Crea quota"}
          </button>
        </form>
        <div style={{ marginTop: 10, fontSize: 12, color: tc.textMuted }}>Quote configurate: {quotas.length}</div>
      </section>

      {/* ─── Quote attive ─── */}
      {quotas.length > 0 && (
        <section style={cardStyle(tc)}>
          <div style={sectionHeader}>
            <strong style={{ fontSize: 14, color: tc.text }}>Quote attive</strong>
          </div>
          <div style={{ overflow: "auto", border: `1px solid ${tc.border}`, borderRadius: 8 }}>
            <table style={tableStyle(tc)}>
              <thead>
                <tr>
                  <th style={thStyle(tc)}>Scope</th>
                  <th style={thStyle(tc)}>Utente</th>
                  <th style={thStyle(tc)}>Progetto</th>
                  <th style={thStyleR(tc)}>Token</th>
                  <th style={thStyleR(tc)}>Costo</th>
                  <th style={thStyle(tc)}>Validità</th>
                  <th style={thStyle(tc)}>Note</th>
                </tr>
              </thead>
              <tbody>
                {quotas.map((q) => {
                  const user = q.user_id ? userById.get(q.user_id) : undefined;
                  const project = q.project_id ? projectById.get(q.project_id) : undefined;
                  return (
                    <tr key={q.id}>
                      <td style={tdStyle(tc)}>{q.scope_type}</td>
                      <td style={tdStyle(tc)}>{user ? user.displayName || user.email : (q.user_id ? q.user_id.substring(0, 8) : "—")}</td>
                      <td style={tdStyle(tc)}>{project ? project.name : (q.project_id ? q.project_id.substring(0, 8) : "—")}</td>
                      <td style={tdStyleR(tc)}>{q.token_limit ? q.token_limit.toLocaleString('it-IT') : "—"}</td>
                      <td style={tdStyleR(tc)}>{q.cost_limit ? `${q.cost_limit.toFixed(2)} ${q.currency || ""}` : "—"}</td>
                      <td style={{ ...tdStyle(tc), fontSize: 11 }}>
                        {new Date(q.valid_from).toLocaleDateString()} → {new Date(q.valid_to).toLocaleDateString()}
                      </td>
                      <td style={{ ...tdStyle(tc), fontSize: 11, color: tc.textMuted }}>{q.note || "—"}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </section>
      )}
    </div>
  );
}

// ── Style helpers ───────────────────────────────────────────────────────────

const sectionHeader: React.CSSProperties = {
  display: "flex",
  alignItems: "center",
  marginBottom: 12,
  flexWrap: "wrap",
};

function cardStyle(tc: Tc): React.CSSProperties {
  return {
    padding: 16,
    borderRadius: 12,
    border: `1px solid ${tc.border}`,
    background: tc.bgCard,
  };
}

function labelStyle(tc: Tc): React.CSSProperties {
  return {
    display: "flex",
    flexDirection: "column",
    gap: 4,
    fontSize: 11,
    color: tc.textMuted,
    fontWeight: 600,
    textTransform: "uppercase",
    letterSpacing: 0.3,
  };
}

function buttonStyle(tc: Tc, primary = false): React.CSSProperties {
  return {
    padding: "10px 14px",
    borderRadius: 8,
    border: `1px solid ${primary ? tc.accent : tc.border}`,
    background: primary ? tc.accent : tc.bgInput,
    color: primary ? "#fff" : tc.text,
    cursor: "pointer",
    fontFamily: "inherit",
    fontSize: 12,
    fontWeight: 700,
    height: 38,
  };
}

function inputStyle(tc: Tc): React.CSSProperties {
  return {
    width: "100%",
    padding: "8px 10px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
    fontFamily: "inherit",
    fontSize: 12,
    boxSizing: "border-box",
    height: 38,
  };
}

function tableStyle(tc: Tc): React.CSSProperties {
  return {
    width: "100%",
    borderCollapse: "collapse",
    fontSize: 12,
    color: tc.text,
  };
}

function thStyle(tc: Tc): React.CSSProperties {
  return {
    textAlign: "left",
    padding: "8px 10px",
    background: tc.bgInput,
    borderBottom: `1px solid ${tc.border}`,
    fontWeight: 600,
    fontSize: 11,
    color: tc.textMuted,
    textTransform: "uppercase",
    letterSpacing: 0.3,
  };
}

function thStyleR(tc: Tc): React.CSSProperties {
  return { ...thStyle(tc), textAlign: "right" };
}

function tdStyle(tc: Tc): React.CSSProperties {
  return {
    padding: "8px 10px",
    borderBottom: `1px solid ${tc.border}`,
  };
}

function tdStyleR(tc: Tc): React.CSSProperties {
  return { ...tdStyle(tc), textAlign: "right", fontVariantNumeric: "tabular-nums" };
}

function tierBadge(tc: Tc, tier: string): React.CSSProperties {
  const colors: Record<string, string> = {
    light: "#10b981",
    medium: "#3b82f6",
    heavy: "#f59e0b",
  };
  const bg = colors[tier] || tc.textMuted;
  return {
    display: "inline-block",
    padding: "2px 8px",
    borderRadius: 10,
    fontSize: 10,
    fontWeight: 700,
    background: bg,
    color: "#fff",
    textTransform: "uppercase",
  };
}
