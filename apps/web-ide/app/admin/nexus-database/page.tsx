"use client";

import { useEffect, useState } from "react";
import { useThemeColors } from "../../../lib/theme";
import { formatBytes, formatDateTime } from "../../../lib/format";
import { AdminPageHeader } from "../../../components/admin/AdminPageHeader";
import { fetchJson } from "../../../lib/api/_shared";

interface TableStats {
  name: string;
  row_count: number | null;
  size_kb: number | null;
  last_updated: string | null;
}

interface DatabaseStats {
  tables: TableStats[];
  stats: {
    total_rows: number;
    database_size_mb: number;
    active_connections: number;
    table_count: number;
    timestamp: string;
  };
}

// Formatter centralizzati in lib/format.ts (regola L / ADR 0026).
// Wrapper locali per preservare le firme attese dai call site di questa pagina:
// formatMB riceve MB, formatKB riceve KB, formatDate accetta null.
const formatDate = (iso: string | null) => formatDateTime(iso);
const formatMB = (mb: number) => formatBytes(mb * 1024 * 1024);
const formatKB = (kb: number | null) =>
  kb === null ? "—" : formatBytes(kb * 1024);

type SortKey = "name" | "row_count" | "size_kb";
type SortDir = "asc" | "desc";

export default function NexusDatabasePage() {
  const tc = useThemeColors();
  const [data, setData] = useState<DatabaseStats | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [sortKey, setSortKey] = useState<SortKey>("size_kb");
  const [sortDir, setSortDir] = useState<SortDir>("desc");
  const [lastRefresh, setLastRefresh] = useState<Date | null>(null);

  const fetchData = async () => {
    setLoading(true);
    setError(null);
    try {
      const json = await fetchJson<DatabaseStats>("/api/admin/nexus-database-stats");
      setData(json);
      setLastRefresh(new Date());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    fetchData();
    const interval = setInterval(fetchData, 30000);
    return () => clearInterval(interval);
  }, []);

  const statCardStyle = {
    background: tc.bgCard ?? tc.bgSidebar,
    border: `1px solid ${tc.border}`,
    borderRadius: 10,
    padding: "18px 22px",
    minWidth: 160,
    flex: 1,
  };

  return (
    <div style={{ color: tc.text, fontFamily: "'JetBrains Mono', monospace" }}>
      <AdminPageHeader
        title="Database Nexus"
        description="Statistiche e stato del database PostgreSQL interno"
        action={
          <button
            onClick={fetchData}
            disabled={loading}
            style={{
              padding: "8px 18px",
              borderRadius: 8,
              background: tc.accent,
              color: "#fff",
              border: "none",
              cursor: loading ? "not-allowed" : "pointer",
              fontSize: 13,
              fontWeight: 600,
              opacity: loading ? 0.6 : 1,
            }}
          >
            {loading ? "Aggiornamento..." : "Aggiorna"}
          </button>
        }
      />

      {lastRefresh && (
        <p style={{ color: tc.textMuted, fontSize: 11, marginBottom: 20 }}>
          Ultimo aggiornamento: {lastRefresh.toLocaleTimeString("it-IT")}
        </p>
      )}

      {error && (
        <div
          style={{
            background: "#3a1010",
            border: "1px solid #7b2020",
            borderRadius: 8,
            padding: "12px 16px",
            color: "#f87171",
            marginBottom: 20,
            fontSize: 13,
          }}
        >
          Errore: {error}
        </div>
      )}

      {data && (
        <>
          {/* Statistiche generali — 4 card con stessa shape (regola L, S25). */}
          <div style={{ display: "flex", gap: 16, flexWrap: "wrap", marginBottom: 28 }}>
            {(
              [
                ["Dimensione DB", formatMB(data.stats.database_size_mb)],
                ["Righe totali", data.stats.total_rows.toLocaleString("it-IT")],
                ["Connessioni attive", String(data.stats.active_connections)],
                ["Tabelle monitorate", String(data.stats.table_count)],
              ] as const
            ).map(([label, value]) => (
              <div key={label} style={statCardStyle}>
                <div style={{ fontSize: 11, color: tc.textMuted, textTransform: "uppercase", letterSpacing: "0.08em" }}>
                  {label}
                </div>
                <div style={{ fontSize: 26, fontWeight: 700, marginTop: 6, color: tc.accent }}>
                  {value}
                </div>
              </div>
            ))}
          </div>

          {/* Ricerca e ordinamento */}
          <div style={{ display: "flex", gap: 10, marginBottom: 12, alignItems: "center", flexWrap: "wrap" }}>
            <input
              type="text"
              placeholder="Cerca tabella..."
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              style={{
                flex: 1,
                minWidth: 180,
                padding: "8px 12px",
                borderRadius: 8,
                border: `1px solid ${tc.border}`,
                background: tc.bgCard ?? tc.bgSidebar,
                color: tc.text,
                fontSize: 13,
                outline: "none",
              }}
            />
            <span style={{ fontSize: 12, color: tc.textMuted }}>
              Ordina per:
            </span>
            {(["size_kb", "row_count", "name"] as SortKey[]).map((k) => {
              const labels: Record<SortKey, string> = { size_kb: "Dimensione", row_count: "Righe", name: "Nome" };
              const active = sortKey === k;
              return (
                <button
                  key={k}
                  onClick={() => {
                    if (sortKey === k) setSortDir((d) => d === "asc" ? "desc" : "asc");
                    else { setSortKey(k); setSortDir("desc"); }
                  }}
                  style={{
                    padding: "6px 12px",
                    borderRadius: 6,
                    border: `1px solid ${active ? tc.accent : tc.border}`,
                    background: active ? `${tc.accent}20` : "transparent",
                    color: active ? tc.accent : tc.textSecondary,
                    cursor: "pointer",
                    fontSize: 12,
                    fontWeight: active ? 600 : 400,
                  }}
                >
                  {labels[k]} {active ? (sortDir === "desc" ? "↓" : "↑") : ""}
                </button>
              );
            })}
          </div>

          {/* Tabella delle tabelle */}
          {(() => {
            const filtered = (data.tables ?? [])
              .filter((t) => !search || t.name.toLowerCase().includes(search.toLowerCase()))
              .sort((a, b) => {
                let av: number | string = 0, bv: number | string = 0;
                if (sortKey === "name") { av = a.name; bv = b.name; }
                else if (sortKey === "row_count") { av = a.row_count ?? -1; bv = b.row_count ?? -1; }
                else if (sortKey === "size_kb") { av = a.size_kb ?? -1; bv = b.size_kb ?? -1; }
                if (typeof av === "string") return sortDir === "asc" ? av.localeCompare(bv as string) : (bv as string).localeCompare(av);
                return sortDir === "asc" ? (av as number) - (bv as number) : (bv as number) - (av as number);
              });
            return (
              <div
                style={{
                  background: tc.bgCard ?? tc.bgSidebar,
                  border: `1px solid ${tc.border}`,
                  borderRadius: 10,
                  overflow: "hidden",
                }}
              >
                <div style={{ padding: "12px 20px", borderBottom: `1px solid ${tc.border}`, display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                  <span style={{ fontWeight: 600, fontSize: 14 }}>Tabelle ({filtered.length})</span>
                  {search && <span style={{ fontSize: 12, color: tc.textMuted }}>su {data.tables.length} totali</span>}
                </div>
                <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 13 }}>
                  <thead>
                    <tr style={{ background: tc.bgHeader ?? tc.bgSidebar, color: tc.textMuted, textTransform: "uppercase", fontSize: 11, letterSpacing: "0.07em" }}>
                      <th style={{ padding: "10px 20px", textAlign: "left", fontWeight: 600 }}>Tabella</th>
                      <th style={{ padding: "10px 20px", textAlign: "right", fontWeight: 600 }}>Righe</th>
                      <th style={{ padding: "10px 20px", textAlign: "right", fontWeight: 600 }}>Dimensione</th>
                      <th style={{ padding: "10px 20px", textAlign: "right", fontWeight: 600 }}>Ultimo aggiorn.</th>
                      <th style={{ padding: "10px 20px", textAlign: "center", fontWeight: 600 }}>Stato</th>
                    </tr>
                  </thead>
                  <tbody>
                    {filtered.map((t, i) => (
                      <tr key={t.name} style={{ borderTop: `1px solid ${tc.border}`, background: i % 2 === 0 ? "transparent" : `${tc.border}20` }}>
                        <td style={{ padding: "10px 20px", fontWeight: 500, color: tc.text }}>
                          <code style={{ fontSize: 12 }}>{t.name}</code>
                        </td>
                        <td style={{ padding: "10px 20px", textAlign: "right", color: tc.textSecondary }}>
                          {t.row_count !== null ? t.row_count.toLocaleString("it-IT") : "—"}
                        </td>
                        <td style={{ padding: "10px 20px", textAlign: "right", color: tc.textMuted, fontSize: 12 }}>
                          {formatKB(t.size_kb)}
                        </td>
                        <td style={{ padding: "10px 20px", textAlign: "right", color: tc.textMuted, fontSize: 12 }}>
                          {formatDate(t.last_updated)}
                        </td>
                        <td style={{ padding: "10px 20px", textAlign: "center" }}>
                          <span
                            style={{ display: "inline-block", width: 8, height: 8, borderRadius: "50%", background: t.size_kb !== null && (t.size_kb ?? 0) > 0 ? "#22c55e" : "#6b7280" }}
                            title={(t.size_kb ?? 0) > 0 ? "Tabella presente" : "Tabella vuota o assente"}
                          />
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            );
          })()}
        </>
      )}

      {loading && !data && (
        <div style={{ textAlign: "center", padding: 60, color: tc.textMuted, fontSize: 13 }}>
          Caricamento statistiche...
        </div>
      )}
    </div>
  );
}
