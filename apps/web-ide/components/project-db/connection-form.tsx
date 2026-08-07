"use client";

import type { Theme } from "../../lib/theme";
import type { ProjectDbTestResult } from "../../lib/api-client";
import type { InitForm, ConnFields } from "./db-helpers";
import { useI18n } from "../../lib/i18n";

/** Field label+input renderer. Punto unico per i 5 input identici di
 *  connessione DB (regola L / ADR 0026). Prima i blocchi
 *  `<div><label/><input/></div>` con stesso styling erano ripetuti 5 volte
 *  e il blocco al riga 99/121 risultava un clone di 100L. */
function DbField({
  tc,
  label,
  type = "text",
  placeholder,
  value,
  onChange,
  fullWidth,
}: {
  tc: Theme;
  label: string;
  type?: "text" | "password";
  placeholder?: string;
  value: string;
  onChange: (v: string) => void;
  fullWidth?: boolean;
}) {
  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 2,
        ...(fullWidth ? { gridColumn: "1 / -1" } : {}),
      }}
    >
      <label style={{ fontSize: 9, color: tc.textMuted }}>{label}</label>
      <input
        type={type}
        placeholder={placeholder}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        style={{
          padding: "4px 6px",
          fontSize: 11,
          background: tc.bgCard,
          color: tc.text,
          border: `1px solid ${tc.border}`,
          borderRadius: 4,
        }}
      />
    </div>
  );
}

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
  const { t } = useI18n();
  return (
    <div style={{ padding: 10, borderBottom: `1px solid ${tc.border}`, background: `${tc.accent}10` }}>
      <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
        <div style={{ fontSize: 11, fontWeight: 600, color: tc.textSecondary }}>
          {isConfigured ? "Modifica configurazione database" : "Nuovo database progetto"}
        </div>
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          <label style={{ fontSize: 10, color: tc.textMuted }}>{t("project-db.nomeConnessione")}</label>
          <input
            type="text"
            placeholder={t("project-db.primaryAnalytics")}
            value={initForm.name}
            onChange={(e) => setInitForm((f) => ({ ...f, name: e.target.value }))}
            style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
          />
          <label style={{ fontSize: 10, color: tc.textMuted }}>{t("project-db.engine")}</label>
          <select
            value={initForm.engine}
            onChange={(e) => setInitForm((f) => ({ ...f, engine: e.target.value }))}
            style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
          >
            <option value="postgres">{t("project-db.postgresql")}</option>
            <option value="mysql">{t("project-db.mysql")}</option>
            <option value="sqlite">{t("project-db.sqlite")}</option>
            <option value="sqlserver">{t("project-db.sqlServer")}</option>
          </select>
          <label style={{ fontSize: 10, color: tc.textMuted }}>{t("project-db.migrationTool")}</label>
          <select
            value={initForm.migration_tool}
            onChange={(e) => setInitForm((f) => ({ ...f, migration_tool: e.target.value }))}
            style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
          >
            <option value="generic_sql">{t("project-db.genericSql")}</option>
            <option value="alembic">{t("project-db.alembic")}</option>
            <option value="prisma">{t("project-db.prisma")}</option>
            <option value="knex">{t("project-db.knex")}</option>
            <option value="flyway">{t("project-db.flyway")}</option>
          </select>
          <label style={{ fontSize: 10, color: tc.textMuted }}>{t("project-db.cartellaMigration")}</label>
          <input
            type="text"
            value={initForm.migration_path}
            onChange={(e) => setInitForm((f) => ({ ...f, migration_path: e.target.value }))}
            style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
          />
          <div style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 4 }}>
            <label style={{ fontSize: 10, color: tc.textMuted, margin: 0 }}>{t("project-db.connessioneDb")}</label>
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
              <DbField
                tc={tc}
                label="Host"
                placeholder={t("project-db.localhost")}
                value={connFields.host}
                onChange={(v) => setConnFields((f) => ({ ...f, host: v }))}
              />
              <DbField
                tc={tc}
                label="Porta"
                placeholder={initForm.engine === "mysql" ? "3306" : initForm.engine === "sqlserver" ? "1433" : "5432"}
                value={connFields.port}
                onChange={(v) => setConnFields((f) => ({ ...f, port: v }))}
              />
              <DbField
                tc={tc}
                label="Database"
                placeholder={t("project-db.nomeDelDbApplicativo")}
                value={connFields.database}
                onChange={(v) => setConnFields((f) => ({ ...f, database: v }))}
                fullWidth
              />
              <DbField
                tc={tc}
                label="Utente"
                placeholder={t("project-db.username")}
                value={connFields.username}
                onChange={(v) => setConnFields((f) => ({ ...f, username: v }))}
              />
              <DbField
                tc={tc}
                label="Password"
                type="password"
                placeholder={t("project-db.password")}
                value={connFields.password}
                onChange={(v) => setConnFields((f) => ({ ...f, password: v }))}
              />
              {buildConnectionString() && (
                <div style={{ gridColumn: "1 / -1", fontSize: 9, color: tc.textMuted, fontFamily: "var(--font-mono)", wordBreak: "break-all", padding: "2px 4px", background: `${tc.border}30`, borderRadius: 3 }}>
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
              <div style={{ fontWeight: 600, marginBottom: 2 }}>{t("project-db.indicatoriRilevati")}</div>
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
            {t("project-db.consentiDdlOverrideManuale")}
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
              {t("project-db.annulla")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
