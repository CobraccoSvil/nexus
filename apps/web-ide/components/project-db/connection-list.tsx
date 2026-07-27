"use client";

import type { Theme } from "../../lib/theme";
import type {
  ProjectDbConfig,
  ProjectDbConnection,
  ProjectDbTestResult,
} from "../../lib/api-client";

interface Props {
  tc: Theme;
  connections: ProjectDbConnection[];
  config: ProjectDbConfig | null;
  busy: boolean;
  showNewMig: boolean;
  connTestingId: string | null;
  connTestResults: Record<string, ProjectDbTestResult>;
  onProvisionWizard: () => void;
  onAddConnection: () => void;
  onTestSaved: (conn: ProjectDbConnection) => void;
  onSetPrimary: (connId: string) => void;
  onEditConfig: (conn: ProjectDbConnection) => void;
  onDeleteConnection: (conn: ProjectDbConnection) => void;
  onToggleNewMig: () => void;
}

/** Lista connessioni DB (card primary/secondary, Testa/Set primary/Modifica/Elimina). */
export function ConnectionList({
  tc,
  connections,
  config,
  busy,
  showNewMig,
  connTestingId,
  connTestResults,
  onProvisionWizard,
  onAddConnection,
  onTestSaved,
  onSetPrimary,
  onEditConfig,
  onDeleteConnection,
  onToggleNewMig,
}: Props) {
  return (
    <div style={{ padding: 10, display: "flex", flexDirection: "column", gap: 6, borderBottom: `1px solid ${tc.border}` }}>
      {/* wrap obbligatorio: in una sidebar da ~195px questa riga chiede 189px di
          min-content contro 159 disponibili. Senza andare a capo i bottoni
          sfondano e la sidebar impone uno scroll orizzontale. */}
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", flexWrap: "wrap", gap: 6 }}>
        <div style={{ fontSize: 11, fontWeight: 600, color: tc.text }}>
          DB del progetto ({connections.length})
        </div>
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
          <button
            type="button"
            onClick={onProvisionWizard}
            disabled={busy}
            style={{
              padding: "2px 8px",
              borderRadius: 4,
              border: "none",
              background: tc.accent,
              color: "#fff",
              cursor: busy ? "not-allowed" : "pointer",
              fontSize: 10,
              fontWeight: 700,
            }}
          >
            Crea database
          </button>
          <button
            type="button"
            onClick={onAddConnection}
            disabled={busy}
            style={{
              padding: "2px 8px",
              borderRadius: 4,
              border: `1px solid ${tc.accent}`,
              background: "transparent",
              color: tc.accent,
              cursor: busy ? "not-allowed" : "pointer",
              fontSize: 10,
              fontWeight: 600,
            }}
          >
            + Aggiungi DB
          </button>
        </div>
      </div>
      {connections.map((c) => (
        <div
          key={c.id}
          style={{
            background: tc.bgCard,
            border: `1px solid ${c.is_primary ? tc.accent : tc.border}`,
            borderRadius: 6,
            padding: "6px 8px",
            display: "flex",
            flexDirection: "column",
            gap: 4,
          }}
        >
          {/* wrap sulla riga contenitore: il gruppo bottoni sotto ha gia' il
              proprio flexWrap, ma non basta finche' e' questa riga a non poter
              andare a capo (misurati 166px di min-content contro 141 utili). */}
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 6, flexWrap: "wrap" }}>
            <div style={{ fontSize: 11, fontWeight: 600, color: tc.text, display: "flex", alignItems: "center", gap: 4 }}>
              {c.name}
              {c.is_primary && (
                <span
                  style={{
                    fontSize: 9,
                    color: tc.accent,
                    border: `1px solid ${tc.accent}`,
                    borderRadius: 3,
                    padding: "0 4px",
                    fontWeight: 600,
                  }}
                >
                  PRIMARY
                </span>
              )}
            </div>
            <div style={{ display: "flex", gap: 4, flexWrap: "wrap" }}>
              <button
                type="button"
                onClick={() => onTestSaved(c)}
                disabled={busy || connTestingId === c.id}
                title="Testa questa connessione"
                style={{
                  padding: "2px 6px",
                  borderRadius: 4,
                  border: `1px solid ${tc.border}`,
                  background: "transparent",
                  color: tc.textSecondary,
                  cursor: busy || connTestingId === c.id ? "not-allowed" : "pointer",
                  fontSize: 10,
                }}
              >
                {connTestingId === c.id ? "Test…" : "Testa"}
              </button>
              {!c.is_primary && (
                <button
                  type="button"
                  onClick={() => onSetPrimary(c.id)}
                  disabled={busy}
                  style={{
                    padding: "2px 6px",
                    borderRadius: 4,
                    border: `1px solid ${tc.border}`,
                    background: "transparent",
                    color: tc.textSecondary,
                    cursor: busy ? "not-allowed" : "pointer",
                    fontSize: 10,
                  }}
                >
                  Set primary
                </button>
              )}
              <button
                type="button"
                onClick={() => onEditConfig(c)}
                disabled={busy}
                style={{
                  padding: "2px 6px",
                  borderRadius: 4,
                  border: `1px solid ${tc.border}`,
                  background: "transparent",
                  color: tc.textSecondary,
                  cursor: busy ? "not-allowed" : "pointer",
                  fontSize: 10,
                }}
              >
                Modifica
              </button>
              <button
                type="button"
                onClick={() => onDeleteConnection(c)}
                disabled={busy}
                style={{
                  padding: "2px 6px",
                  borderRadius: 4,
                  border: `1px solid ${tc.error}40`,
                  background: "transparent",
                  color: tc.error,
                  cursor: busy ? "not-allowed" : "pointer",
                  fontSize: 10,
                }}
              >
                Elimina
              </button>
            </div>
          </div>
          {connTestResults[c.id] && (
            <div
              style={{
                fontSize: 10,
                padding: "3px 6px",
                borderRadius: 4,
                background: connTestResults[c.id].ok ? "#22c55e20" : `${tc.error}20`,
                border: `1px solid ${connTestResults[c.id].ok ? "#22c55e40" : `${tc.error}40`}`,
                color: connTestResults[c.id].ok ? "#22c55e" : tc.error,
                whiteSpace: "pre-wrap",
                wordBreak: "break-word",
              }}
            >
              {connTestResults[c.id].ok
                ? `OK · ${connTestResults[c.id].server_version?.slice(0, 60) ?? ""} · ${connTestResults[c.id].table_count ?? 0} tabelle · ${connTestResults[c.id].latency_ms ?? 0}ms`
                : `Errore: ${connTestResults[c.id].error ?? "sconosciuto"}`}
              {!connTestResults[c.id].ok && connTestResults[c.id].hint && (
                <div style={{
                  marginTop: 4,
                  fontSize: 10,
                  color: tc.textMuted,
                  borderTop: `1px solid ${tc.border}`,
                  paddingTop: 4,
                }}>
                  {connTestResults[c.id].hint}
                </div>
              )}
            </div>
          )}
          <div style={{ fontSize: 10, color: tc.textMuted, lineHeight: 1.5 }}>
            <div>
              <span style={{ color: tc.textSecondary }}>Engine:</span> {c.engine ?? "—"}
              {c.hosting_mode ? ` · ${c.hosting_mode}` : ""}
            </div>
            <div>
              <span style={{ color: tc.textSecondary }}>Tool:</span> {c.migration_tool ?? "—"}
              {c.migration_path ? ` · ${c.migration_path}` : ""}
            </div>
            <div>
              <span style={{ color: tc.textSecondary }}>DDL override:</span>{" "}
              {c.allow_ddl_override ? "abilitato" : "disabilitato"}
            </div>
          </div>
        </div>
      ))}
      <div style={{ display: "flex", gap: 6 }}>
        <button
          type="button"
          onClick={onToggleNewMig}
          disabled={busy || !config?.allow_ddl_override}
          title={config?.allow_ddl_override ? "Crea nuova migrazione" : "Abilita allow_ddl_override per creare migrazioni manuali"}
          style={{
            flex: 1,
            padding: "6px 8px",
            borderRadius: 6,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
            color: config?.allow_ddl_override ? tc.text : tc.textMuted,
            cursor: config?.allow_ddl_override ? "pointer" : "not-allowed",
            fontSize: 12,
            fontWeight: 600,
          }}
        >
          {showNewMig ? "Chiudi" : "Nuova migrazione"}
        </button>
      </div>
    </div>
  );
}
