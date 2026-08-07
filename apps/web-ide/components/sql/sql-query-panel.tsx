"use client";

import dynamic from "next/dynamic";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTheme, useThemeColors } from "../../lib/theme";
import {
  executeProjectDbQuery,
  listProjectDbConnections,
  type ProjectDbConnection,
  type SqlExecuteResult,
  type UserProjectDetails,
} from "../../lib/api-client";
import { AutoWidthSelect } from "../auto-width-select";
import { useI18n } from "../../lib/i18n";

const MonacoEditor = dynamic(
  async () => (await import("@monaco-editor/react")).default,
  { ssr: false },
);

interface SqlQueryPanelProps {
  project: UserProjectDetails | null;
}

interface HistoryEntry {
  id: string;
  sql: string;
  kind: string;
  rows: number;
  duration_ms: number;
  at: number;
}

const PLACEHOLDER_SQL = "-- Scrivi una query SQL ed esegui con Ctrl+Enter\n-- Esempio: SELECT * FROM users LIMIT 10;\n\n";

export function SqlQueryPanel({ project }: SqlQueryPanelProps) {
  const { t } = useI18n();
  const { resolved } = useTheme();
  const tc = useThemeColors();
  const [sql, setSql] = useState<string>(PLACEHOLDER_SQL);
  const [result, setResult] = useState<SqlExecuteResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [connections, setConnections] = useState<ProjectDbConnection[]>([]);
  const [selectedConnection, setSelectedConnection] = useState<string>("");
  const monacoRef = useRef<unknown | null>(null);

  const projectId = project?.id ?? null;

  // Carica le connessioni DB del progetto per popolare il dropdown.
  // Quando ce ne sono >1 l'utente puo' scegliere su quale DB eseguire la
  // query senza dover spostare il flag is_primary nel pannello Database.
  useEffect(() => {
    if (!projectId) {
      setConnections([]);
      setSelectedConnection("");
      return;
    }
    let cancelled = false;
    void (async () => {
      try {
        const res = await listProjectDbConnections(projectId);
        if (cancelled) return;
        const conns = res.connections ?? [];
        setConnections(conns);
        // Default: la primary; se non c'e', la prima.
        const primary = conns.find((c) => c.is_primary) ?? conns[0];
        setSelectedConnection(primary?.name ?? "");
      } catch {
        if (cancelled) return;
        setConnections([]);
        setSelectedConnection("");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [projectId]);

  // Sincronizza tema Monaco.
  useEffect(() => {
    const m = monacoRef.current as { editor: { setTheme: (n: string) => void } } | null;
    if (!m) return;
    m.editor.setTheme(resolved === "dark" ? "vs-dark" : "vs");
  }, [resolved]);

  const runQuery = useCallback(async () => {
    if (!projectId) {
      setError("Nessun progetto attivo: seleziona un progetto prima di eseguire SQL.");
      return;
    }
    const trimmed = sql.trim();
    if (!trimmed || trimmed.startsWith("--") && trimmed.split("\n").every(l => l.trim().startsWith("--") || !l.trim())) {
      setError("Inserisci una query SQL non vuota.");
      return;
    }
    setLoading(true);
    setError(null);
    try {
      // Se l'utente ha selezionato una connessione esplicita, la passiamo
      // al backend. Se vuota -> backend usa la primary (comportamento
      // storico).
      const res = await executeProjectDbQuery(
        projectId,
        trimmed,
        undefined,
        undefined,
        selectedConnection || undefined,
      );
      setResult(res);
      // Push nella cronologia locale (max 20).
      const kind = res.mode === "read" ? res.statement_kind : res.statement_kind;
      const rows = res.mode === "read" ? res.row_count : res.rows_affected;
      setHistory((h) =>
        [
          {
            id: `${Date.now()}`,
            sql: trimmed,
            kind,
            rows,
            duration_ms: res.duration_ms,
            at: Date.now(),
          },
          ...h,
        ].slice(0, 20),
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      setResult(null);
    } finally {
      setLoading(false);
    }
  }, [projectId, sql, selectedConnection]);

  // Listener globale: la chat puo' aprire il pannello e pre-compilare l'editor.
  useEffect(() => {
    const handler = (ev: Event) => {
      const ce = ev as CustomEvent<{ sql?: string; autoRun?: boolean }>;
      if (typeof ce.detail?.sql === "string") {
        setSql(ce.detail.sql);
        setError(null);
        if (ce.detail.autoRun) {
          // Defer per assicurarsi che lo stato sia applicato prima di runQuery.
          setTimeout(() => void runQuery(), 0);
        }
      }
    };
    window.addEventListener("nexus:sql:set-content", handler);
    return () => window.removeEventListener("nexus:sql:set-content", handler);
  }, [runQuery]);

  // Hotkey Ctrl/Cmd+Enter dentro Monaco.
  const onMonacoMount = useCallback(
    (editor: unknown, monaco: unknown) => {
      monacoRef.current = monaco;
      const m = monaco as {
        KeyMod: { CtrlCmd: number };
        KeyCode: { Enter: number };
        editor: { setTheme: (n: string) => void };
      };
      const ed = editor as {
        addCommand: (key: number, cb: () => void) => void;
      };
      ed.addCommand(m.KeyMod.CtrlCmd | m.KeyCode.Enter, () => {
        void runQuery();
      });
      m.editor.setTheme(resolved === "dark" ? "vs-dark" : "vs");
    },
    [runQuery, resolved],
  );

  const isDdl = useMemo(() => {
    const t = sql.trim().toLowerCase();
    return (
      t.startsWith("create") ||
      t.startsWith("alter") ||
      t.startsWith("drop") ||
      t.startsWith("truncate") ||
      t.startsWith("rename")
    );
  }, [sql]);

  return (
    <div
      style={{
        height: "100%",
        display: "grid",
        gridTemplateRows: "32px 1fr 6px 1fr",
        gridTemplateColumns: "1fr",
        background: tc.bg,
        color: tc.text,
        minHeight: 0,
        overflow: "hidden",
      }}
    >
      {/* Toolbar */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          padding: "0 8px",
          borderBottom: `1px solid ${tc.border}`,
          fontSize: 12,
          // Robustezza overflow: nei 32px fissi della riga i figli non devono
          // andare a capo. Il contenitore puo' restringersi (minWidth:0) e
          // nasconde l'eccedenza; il nome progetto tronca con ellissi.
          minWidth: 0,
          overflow: "hidden",
        }}
      >
        <span
          style={{
            fontWeight: 600,
            minWidth: 0,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
          title={`SQL · ${project?.name ?? "(nessun progetto)"}`}
        >
          SQL · {project?.name ?? "(nessun progetto)"}
        </span>
        {connections.length > 0 && (
          <>
            <span style={{ color: tc.textMuted, marginLeft: 4, flexShrink: 0 }}>DB:</span>
            <AutoWidthSelect
              value={selectedConnection}
              options={connections.map((c) => ({
                value: c.name,
                label: `${c.name}${c.is_primary ? " (primary)" : ""}${c.engine ? ` · ${c.engine}` : ""}`,
              }))}
              onChange={(value) => setSelectedConnection(value)}
              disabled={loading}
              title={
                connections.length === 1
                  ? "Una sola connessione nel progetto"
                  : "Scegli su quale connessione del progetto eseguire la query (gestione multi-DB)"
              }
              style={{
                background: tc.bgInput,
                color: tc.text,
                border: `1px solid ${tc.border}`,
                borderRadius: 3,
                fontSize: 11,
                padding: "1px 6px",
                cursor: connections.length > 1 ? "pointer" : "default",
                flexShrink: 0,
              }}
            />
          </>
        )}
        <button
          type="button"
          onClick={() => void runQuery()}
          disabled={loading || !projectId}
          style={{
            marginLeft: "auto",
            padding: "3px 10px",
            background: loading ? tc.textMuted : tc.accent,
            color: "#fff",
            border: "none",
            borderRadius: 4,
            cursor: loading || !projectId ? "not-allowed" : "pointer",
            fontSize: 12,
            flexShrink: 0,
            whiteSpace: "nowrap",
          }}
          title={t("sql.eseguiCtrlEnter")}
        >
          {loading ? "Eseguo…" : "Esegui (Ctrl+Enter)"}
        </button>
        {isDdl && (
          <span
            style={{
              padding: "2px 6px",
              background: "#7a5b00",
              color: "#fff8d6",
              borderRadius: 3,
              fontSize: 11,
              // Badge compatto: non deve mandare a capo ne' comprimere gli altri
              // controlli; su viewport stretti si tronca con ellissi.
              minWidth: 0,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
            title={t("sql.leQueryDdlCreate")}
          >
            schema-change → KB + migration
          </span>
        )}
      </div>

      {/* Editor SQL */}
      <div style={{ minHeight: 0, overflow: "hidden" }}>
        <MonacoEditor
          path="nexus-sql-panel.sql"
          language="sql"
          value={sql}
          onChange={(v) => setSql(v ?? "")}
          onMount={onMonacoMount}
          options={{
            minimap: { enabled: false },
            fontSize: 13,
            lineHeight: 20,
            fontFamily: 'var(--font-mono)',
            automaticLayout: true,
            wordWrap: "on",
            tabSize: 2,
            scrollBeyondLastLine: false,
          }}
        />
      </div>

      {/* Separatore visuale */}
      <div style={{ background: tc.border }} />

      {/* Risultati */}
      <div style={{ minHeight: 0, overflow: "auto", padding: 8, fontSize: 12 }}>
        {error && (
          <div
            style={{
              padding: 8,
              background: "#3b1d1d",
              color: "#ffb4b4",
              border: "1px solid #6b2828",
              borderRadius: 4,
              whiteSpace: "pre-wrap",
              fontFamily: 'var(--font-mono)',
              marginBottom: 8,
            }}
          >
            {error}
          </div>
        )}

        {!error && result && result.mode === "read" && (
          <ReadResultGrid result={result} tc={tc} />
        )}

        {!error && result && result.mode === "write" && (
          <div style={{ padding: 8, background: tc.bgCard, borderRadius: 4 }}>
            <div>
              <strong>{result.statement_kind.toUpperCase()}</strong> · {result.rows_affected} righe modificate · {result.duration_ms} ms
            </div>
            {result.hint && <div style={{ marginTop: 4, color: tc.textMuted }}>{result.hint}</div>}
          </div>
        )}

        {!error && !result && (
          <div style={{ color: tc.textMuted, padding: 8 }}>
            {t("sql.eseguiUnaQueryCtrl")}
          </div>
        )}

        {history.length > 0 && (
          <div style={{ marginTop: 12, borderTop: `1px solid ${tc.border}`, paddingTop: 8 }}>
            <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 4 }}>
              {t("sql.cronologiaQuestaSessione")}
            </div>
            {history.map((h) => (
              <button
                type="button"
                key={h.id}
                onClick={() => setSql(h.sql)}
                title={t("sql.clickPerRicaricareNell")}
                style={{
                  display: "block",
                  width: "100%",
                  textAlign: "left",
                  padding: "4px 6px",
                  background: "transparent",
                  border: "none",
                  borderBottom: `1px solid ${tc.border}`,
                  color: tc.text,
                  fontFamily: 'var(--font-mono)',
                  fontSize: 11,
                  cursor: "pointer",
                }}
              >
                <span style={{ color: tc.accent }}>{h.kind}</span> · {h.rows} rows · {h.duration_ms}ms ·{" "}
                <span style={{ color: tc.textMuted }}>
                  {h.sql.replace(/\s+/g, " ").slice(0, 120)}
                  {h.sql.length > 120 ? "…" : ""}
                </span>
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function ReadResultGrid({
  result,
  tc,
}: {
  result: Extract<SqlExecuteResult, { mode: "read" }>;
  tc: ReturnType<typeof useThemeColors>;
}) {
  const { columns, rows, row_count, truncated, duration_ms, statement_kind } = result;

  if (columns.length === 0 || rows.length === 0) {
    return (
      <div style={{ padding: 8, background: tc.bgCard, borderRadius: 4 }}>
        <strong>{statement_kind.toUpperCase()}</strong> · 0 righe · {duration_ms} ms
      </div>
    );
  }

  return (
    <div>
      <div style={{ marginBottom: 6, color: tc.textMuted, fontSize: 11 }}>
        <strong style={{ color: tc.text }}>{row_count}</strong> righe · {duration_ms} ms
        {truncated && (
          <span style={{ marginLeft: 8, color: "#d99a00" }}>
            (truncato a 1000 righe; aumenta max_rows o aggiungi LIMIT)
          </span>
        )}
      </div>
      <div style={{ overflow: "auto", border: `1px solid ${tc.border}`, borderRadius: 4 }}>
        <table style={{ borderCollapse: "collapse", width: "100%", fontSize: 12 }}>
          <thead>
            <tr style={{ background: tc.bgCard }}>
              {columns.map((c) => (
                <th
                  key={c.name}
                  style={{
                    textAlign: "left",
                    padding: "4px 8px",
                    borderBottom: `1px solid ${tc.border}`,
                    fontWeight: 600,
                    whiteSpace: "nowrap",
                  }}
                >
                  {c.name}
                  <span style={{ color: tc.textMuted, fontWeight: 400, marginLeft: 6, fontSize: 10 }}>
                    {c.type}
                  </span>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map((row, i) => (
              <tr key={i} style={{ background: i % 2 ? tc.bg : tc.bgCard }}>
                {columns.map((c) => (
                  <td
                    key={c.name}
                    style={{
                      padding: "3px 8px",
                      borderBottom: `1px solid ${tc.border}`,
                      fontFamily: 'var(--font-mono)',
                      whiteSpace: "nowrap",
                      maxWidth: 320,
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                    }}
                    title={formatCell(row[c.name])}
                  >
                    {formatCell(row[c.name])}
                  </td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function formatCell(v: unknown): string {
  if (v === null || v === undefined) return "∅";
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}
