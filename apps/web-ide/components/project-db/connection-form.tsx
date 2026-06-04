"use client";

import type { Theme } from "../../lib/theme";
import type { ProjectDbTestResult } from "../../lib/api-client";
import type { InitForm, ConnFields } from "./db-helpers";

interface Props {
  tc: Theme;
  isConfigured: boolean;
  initForm: InitForm;
  setInitForm: (updater: (f: InitForm) => InitForm) => void;
  connFields: ConnFields;
  setConnFields: (updater: (f: ConnFields) => ConnFields) => void;
  useConnFields: boolean;
  setUseConnFields: (v: boolean) => void;
  busy: boolean;
  testResult: ProjectDbTestResult | null;
  detectHints: string[] | null;
  buildConnectionString: () => string;
  onTestConnection: () => void;
  onInit: () => void;
  onCancel: () => void;
}

/** Form di inizializzazione/modifica connessione DB (host/port/db/user/pwd, test, DDL override). */
export function ConnectionForm({
  tc,
  isConfigured,
  initForm,
  setInitForm,
  connFields,
  setConnFields,
  useConnFields,
  setUseConnFields,
  busy,
  testResult,
  detectHints,
  buildConnectionString,
  onTestConnection,
  onInit,
  onCancel,
}: Props) {
  return (
    <div style={{ padding: 10, borderBottom: `1px solid ${tc.border}`, background: `${tc.accent}10` }}>
      <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
        <div style={{ fontSize: 11, fontWeight: 600, color: tc.textSecondary }}>
          {isConfigured ? "Modifica configurazione database" : "Nuovo database progetto"}
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          <label style={{ fontSize: 10, color: tc.textMuted }}>Nome connessione</label>
          <input
            type="text"
            placeholder="primary, analytics, ..."
            value={initForm.name}
            onChange={(e) => setInitForm((f) => ({ ...f, name: e.target.value }))}
            style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
          />
          <label style={{ fontSize: 10, color: tc.textMuted }}>Engine</label>
          <select
            value={initForm.engine}
            onChange={(e) => setInitForm((f) => ({ ...f, engine: e.target.value }))}
            style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
          >
            <option value="postgres">PostgreSQL</option>
            <option value="mysql">MySQL</option>
            <option value="sqlite">SQLite</option>
            <option value="sqlserver">SQL Server</option>
          </select>
          <label style={{ fontSize: 10, color: tc.textMuted }}>Migration tool</label>
          <select
            value={initForm.migration_tool}
            onChange={(e) => setInitForm((f) => ({ ...f, migration_tool: e.target.value }))}
            style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
          >
            <option value="generic_sql">Generic SQL</option>
            <option value="alembic">Alembic</option>
            <option value="prisma">Prisma</option>
            <option value="knex">Knex</option>
            <option value="flyway">Flyway</option>
          </select>
          <label style={{ fontSize: 10, color: tc.textMuted }}>Cartella migration</label>
          <input
            type="text"
            value={initForm.migration_path}
            onChange={(e) => setInitForm((f) => ({ ...f, migration_path: e.target.value }))}
            style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
          />
          <div style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 4 }}>
            <label style={{ fontSize: 10, color: tc.textMuted, margin: 0 }}>Connessione DB</label>
            <button
              type="button"
              onClick={() => setUseConnFields(!useConnFields)}
              style={{ fontSize: 9, color: tc.accent ?? "#4a9eff", background: "none", border: "none", cursor: "pointer", padding: 0 }}
            >
              {useConnFields ? "Usa stringa raw" : "Usa campi separati"}
            </button>
          </div>
          {useConnFields && initForm.engine !== "sqlite" ? (
            <div style={{ display: "grid", gridTemplateColumns: "1fr 80px", gap: 4 }}>
              <div style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                <label style={{ fontSize: 9, color: tc.textMuted }}>Host</label>
                <input
                  type="text"
                  placeholder="localhost"
                  value={connFields.host}
                  onChange={(e) => setConnFields((f) => ({ ...f, host: e.target.value }))}
                  style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
                />
              </div>
              <div style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                <label style={{ fontSize: 9, color: tc.textMuted }}>Porta</label>
                <input
                  type="text"
                  placeholder={initForm.engine === "mysql" ? "3306" : initForm.engine === "sqlserver" ? "1433" : "5432"}
                  value={connFields.port}
                  onChange={(e) => setConnFields((f) => ({ ...f, port: e.target.value }))}
                  style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
                />
              </div>
              <div style={{ gridColumn: "1 / -1", display: "flex", flexDirection: "column", gap: 2 }}>
                <label style={{ fontSize: 9, color: tc.textMuted }}>Database</label>
                <input
                  type="text"
                  placeholder="nome del DB applicativo (es. myapp_dev)"
                  value={connFields.database}
                  onChange={(e) => setConnFields((f) => ({ ...f, database: e.target.value }))}
                  style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
                />
              </div>
              <div style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                <label style={{ fontSize: 9, color: tc.textMuted }}>Utente</label>
                <input
                  type="text"
                  placeholder="username"
                  value={connFields.username}
                  onChange={(e) => setConnFields((f) => ({ ...f, username: e.target.value }))}
                  style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
                />
              </div>
              <div style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                <label style={{ fontSize: 9, color: tc.textMuted }}>Password</label>
                <input
                  type="password"
                  placeholder="password"
                  value={connFields.password}
                  onChange={(e) => setConnFields((f) => ({ ...f, password: e.target.value }))}
                  style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
                />
              </div>
              {buildConnectionString() && (
                <div style={{ gridColumn: "1 / -1", fontSize: 9, color: tc.textMuted, fontFamily: "monospace", wordBreak: "break-all", padding: "2px 4px", background: `${tc.border}30`, borderRadius: 3 }}>
                  {buildConnectionString().replace(/[Pp]assword=([^;,"']+)/g, "Password=***")}
                </div>
              )}
            </div>
          ) : (
            <input
              type="text"
              placeholder={
                initForm.engine === "sqlserver"
                  ? "Server=host,1433;Database=db;User Id=user;Password=pwd;TrustServerCertificate=True;"
                  : initForm.engine === "mysql"
                  ? "mysql://user:pass@host:3306/db"
                  : initForm.engine === "sqlite"
                  ? "/percorso/al/file.db"
                  : "Host=host;Port=5432;Database=db;Username=user;Password=pass"
              }
              value={initForm.connection_string}
              onChange={(e) => setInitForm((f) => ({ ...f, connection_string: e.target.value }))}
              style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
            />
          )}
          <button
            type="button"
            onClick={onTestConnection}
            disabled={busy}
            style={{
              padding: "4px 8px",
              borderRadius: 6,
              border: `1px solid ${tc.border}`,
              background: tc.bgCard,
              color: tc.text,
              cursor: busy ? "not-allowed" : "pointer",
              fontSize: 11,
              alignSelf: "flex-start",
            }}
          >
            {busy ? "Test…" : "Testa connessione"}
          </button>
          {testResult && (
            <div
              style={{
                fontSize: 10,
                padding: "4px 6px",
                borderRadius: 4,
                background: testResult.ok ? "#22c55e20" : `${tc.error}20`,
                border: `1px solid ${testResult.ok ? "#22c55e40" : `${tc.error}40`}`,
                color: testResult.ok ? "#22c55e" : tc.error,
                whiteSpace: "pre-wrap",
                wordBreak: "break-word",
              }}
            >
              {testResult.ok
                ? `OK · ${testResult.server_version?.slice(0, 60) ?? ""} · ${testResult.table_count ?? 0} tabelle · ${testResult.latency_ms ?? 0}ms`
                : `Errore: ${testResult.error ?? "sconosciuto"}`}
              {!testResult.ok && testResult.hint && (
                <div style={{
                  marginTop: 4,
                  fontSize: 10,
                  color: tc.textMuted,
                  borderTop: `1px solid ${tc.border}`,
                  paddingTop: 4,
                }}>
                  {testResult.hint}
                </div>
              )}
            </div>
          )}
          {detectHints && detectHints.length > 0 && (
            <div style={{ fontSize: 10, color: tc.textMuted, lineHeight: 1.4 }}>
              <div style={{ fontWeight: 600, marginBottom: 2 }}>Indicatori rilevati:</div>
              {detectHints.map((h, i) => (
                <div key={i}>• {h}</div>
              ))}
            </div>
          )}
          <label style={{ fontSize: 11, color: tc.textSecondary, display: "flex", alignItems: "center", gap: 6 }}>
            <input
              type="checkbox"
              checked={initForm.allow_ddl_override}
              onChange={(e) => setInitForm((f) => ({ ...f, allow_ddl_override: e.target.checked }))}
            />
            Consenti DDL override manuale
          </label>
          <div style={{ display: "flex", gap: 6 }}>
            <button
              type="button"
              onClick={onInit}
              disabled={busy}
              style={{
                flex: 1,
                padding: "6px 8px",
                borderRadius: 6,
                border: "none",
                background: tc.accent,
                color: "#fff",
                cursor: busy ? "not-allowed" : "pointer",
                fontSize: 12,
                fontWeight: 600,
              }}
            >
              {busy ? "…" : "Salva"}
            </button>
            <button
              type="button"
              onClick={onCancel}
              disabled={busy}
              style={{
                padding: "6px 8px",
                borderRadius: 6,
                border: `1px solid ${tc.border}`,
                background: tc.bgCard,
                color: tc.text,
                cursor: "pointer",
                fontSize: 12,
              }}
            >
              Annulla
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
