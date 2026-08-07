"use client";

import type { Theme } from "../../lib/theme";
import {
  testProjectDbConnection,
  type ProjectDbTestResult,
} from "../../lib/api-client";
import {
  categorizeDbError,
  parseConnPartsForActions,
  alternativeHostsFor,
  parseConnectionString,
  type DbErrorCategory,
  type DetectedConfig,
  type InitForm,
  type ConnFields,
} from "./db-helpers";
import { useI18n } from "../../lib/i18n";

interface Props {
  tc: Theme;
  projectId: string;
  detectedConfig: DetectedConfig;
  detectedTestResult: ProjectDbTestResult | null;
  loading: boolean;
  busy: boolean;
  setBusy: (v: boolean) => void;
  setDetectedConfig: (c: DetectedConfig) => void;
  setDetectedTestResult: (r: ProjectDbTestResult | null) => void;
  setInitForm: (updater: (f: InitForm) => InitForm) => void;
  setUseConnFields: (v: boolean) => void;
  setConnFields: (v: ConnFields) => void;
  setDetectHints: (v: string[] | null) => void;
  setTestResult: (v: ProjectDbTestResult | null) => void;
  setShowInit: (v: boolean) => void;
  setActionMsg: (v: string | null) => void;
  onReload: () => void;
  onTestDetected: () => void;
}

/** Sezione "Rilevato dai sorgenti del progetto" — separata dal DB interno di Nexus. */
export function DetectConfig({
  tc,
  projectId,
  detectedConfig,
  detectedTestResult,
  loading,
  busy,
  setBusy,
  setDetectedConfig,
  setDetectedTestResult,
  setInitForm,
  setUseConnFields,
  setConnFields,
  setDetectHints,
  setTestResult,
  setShowInit,
  setActionMsg,
  onReload,
  onTestDetected,
}: Props) {
  const { t } = useI18n();
  return (
    <div
      style={{
        padding: "8px 10px",
        borderBottom: `1px solid ${tc.border}`,
        display: "flex",
        flexDirection: "column",
        gap: 4,
      }}
    >
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8 }}>
        <div style={{ fontSize: 10, fontWeight: 700, color: tc.textSecondary, textTransform: "uppercase", letterSpacing: "0.05em" }}>
          {t("project-db.rilevatoDaiSorgentiDel")}
        </div>
        <button
          type="button"
          onClick={onReload}
          title={t("project-db.riAnalizzaIFile")}
          disabled={loading}
          style={{
            padding: "1px 8px", fontSize: 10,
            borderRadius: 3, border: `1px solid ${tc.border}`,
            background: "transparent", color: tc.textMuted,
            cursor: loading ? "default" : "pointer",
            fontFamily: 'var(--font-mono)',
            opacity: loading ? 0.5 : 1,
          }}
        >
          ↺ {loading ? "..." : "re-rileva"}
        </button>
      </div>
      <div
        style={{
          background: `${tc.accent}10`,
          border: `1px solid ${tc.accent}30`,
          borderRadius: 6,
          padding: "6px 8px",
          display: "flex",
          flexDirection: "column",
          gap: 3,
        }}
      >
        <div style={{ fontSize: 10, color: tc.textMuted, lineHeight: 1.6 }}>
          {detectedConfig.engine && (
            <div>
              <span style={{ color: tc.textSecondary }}>{t("project-db.engine2")}</span>{" "}
              <span style={{ color: tc.text, fontWeight: 600 }}>{detectedConfig.engine}</span>
              {detectedConfig.hosting_mode ? ` · ${detectedConfig.hosting_mode}` : ""}
            </div>
          )}
          {detectedConfig.migration_tool && (
            <div>
              <span style={{ color: tc.textSecondary }}>{t("project-db.tool")}</span> {detectedConfig.migration_tool}
              {detectedConfig.migration_path ? ` · ${detectedConfig.migration_path}` : ""}
            </div>
          )}
          {detectedConfig.connection_string && (
            <div style={{ fontFamily: "var(--font-mono)", fontSize: 9, color: tc.textMuted, wordBreak: "break-all", marginTop: 2 }}>
              <span style={{ color: tc.textSecondary }}>{t("project-db.connessione")}</span>{" "}
              {/* Oscura password nella connection string per sicurezza */}
              {detectedConfig.connection_string.replace(/[Pp]assword=([^;,"']+)/g, "Password=***").replace(/:([^:@]+)@/, ":***@")}
            </div>
          )}
          {detectedConfig.hints.length > 0 && (
            <div style={{ marginTop: 2, color: tc.textMuted, fontSize: 9 }}>
              {detectedConfig.hints.slice(0, 4).join(" · ")}
            </div>
          )}
        </div>
        <button
          type="button"
          onClick={() => {
            setInitForm((f) => ({
              ...f,
              name: "primary",
              engine: detectedConfig.engine ?? f.engine,
              hosting_mode: detectedConfig.hosting_mode ?? f.hosting_mode,
              migration_tool: detectedConfig.migration_tool ?? f.migration_tool,
              migration_path: detectedConfig.migration_path ?? f.migration_path,
              connection_string: detectedConfig.connection_string ?? f.connection_string,
            }));
            const parsed = detectedConfig.connection_string
              ? parseConnectionString(detectedConfig.connection_string)
              : null;
            if (parsed) {
              setUseConnFields(true);
              setConnFields({
                host: parsed.host,
                port: parsed.port || "5432",
                database: parsed.database,
                username: parsed.username,
                password: parsed.password,
              });
            }
            setDetectHints(detectedConfig.hints);
            setTestResult(null);
            setShowInit(true);
          }}
          style={{
            alignSelf: "flex-start",
            padding: "2px 8px",
            borderRadius: 4,
            border: `1px solid ${tc.accent}`,
            background: "transparent",
            color: tc.accent,
            cursor: "pointer",
            fontSize: 10,
            fontWeight: 600,
            marginTop: 2,
          }}
        >
          {t("project-db.usaQuestaConfigurazione")}
        </button>

        <button
          type="button"
          onClick={onTestDetected}
          disabled={busy || !detectedConfig.connection_string}
          style={{
            alignSelf: "flex-start",
            padding: "2px 8px",
            borderRadius: 4,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
            color: tc.textSecondary,
            cursor: busy ? "not-allowed" : "pointer",
            fontSize: 10,
            fontWeight: 600,
            marginTop: 2,
          }}
          title={t("project-db.esegueUnTestDi")}
        >
          {busy ? "Test…" : "Testa config rilevata"}
        </button>

        {detectedTestResult && (
          <div
            style={{
              marginTop: 4,
              fontSize: 10,
              padding: "4px 6px",
              borderRadius: 4,
              background: detectedTestResult.ok ? "#22c55e20" : `${tc.error}20`,
              border: `1px solid ${detectedTestResult.ok ? "#22c55e40" : `${tc.error}40`}`,
              color: detectedTestResult.ok ? "#22c55e" : tc.error,
              whiteSpace: "pre-wrap",
              wordBreak: "break-word",
            }}
          >
            {detectedTestResult.ok
              ? `OK · ${detectedTestResult.server_version?.slice(0, 60) ?? ""} · ${detectedTestResult.table_count ?? 0} tabelle · ${detectedTestResult.latency_ms ?? 0}ms`
              : `Errore: ${detectedTestResult.error ?? "sconosciuto"}`}
            {!detectedTestResult.ok && detectedTestResult.hint && (
              <div
                style={{
                  marginTop: 4,
                  fontSize: 10,
                  color: tc.textMuted,
                  borderTop: `1px solid ${tc.border}`,
                  paddingTop: 4,
                }}
              >
                {detectedTestResult.hint}
              </div>
            )}
          </div>
        )}

        {/* Azioni suggerite quando il test della config rilevata fallisce.
            La categorizzazione preferisce `category` del backend (SQLSTATE per
            postgres, regola M) e ricade su categorizeDbError (parsing testo)
            solo per i motori che non la emettono ancora. Le azioni sono
            context-aware: per "unreachable" tentiamo automaticamente host
            alternativi sulla LAN; per "no_database" generiamo un prompt agente
            per creare il DB; per "tables_missing" suggeriamo le migrazioni. */}
        {detectedTestResult && !detectedTestResult.ok && detectedConfig?.connection_string && (() => {
          const category =
            detectedTestResult.category ??
            categorizeDbError(detectedTestResult.error, detectedTestResult.table_count);
          const parts = parseConnPartsForActions(detectedConfig.connection_string ?? "");
          const dbName = parts?.database || "<nome_db>";
          const altHosts = parts ? alternativeHostsFor(parts.host) : [];
          const labelByCategory: Record<DbErrorCategory, string> = {
            unreachable: "Host non raggiungibile",
            no_database: `Database "${dbName}" non esiste sul server`,
            auth_failed: "Credenziali non valide",
            tables_missing: "Connesso ma nessuna tabella",
            unknown: "Errore non classificato",
          };
          const tryAlternativeHost = async (altHost: string) => {
            if (!projectId || !parts) return;
            const newConn = (detectedConfig.connection_string ?? "")
              .replace(`@${parts.host}`, `@${altHost}`)
              .replace(`//${parts.host}`, `//${altHost}`);
            setBusy(true);
            setDetectedTestResult(null);
            try {
              const res = await testProjectDbConnection(projectId, {
                engine: detectedConfig?.engine ?? undefined,
                connection_string: newConn,
              });
              setDetectedTestResult(res);
              if (res.ok) {
                // Salva il connection_string corretto come override
                setDetectedConfig({
                  ...detectedConfig,
                  connection_string: newConn,
                });
              }
            } catch (e) {
              setDetectedTestResult({
                ok: false,
                error: e instanceof Error ? e.message : `Test fallito su ${altHost}`,
              });
            } finally {
              setBusy(false);
            }
          };
          return (
            <div
              style={{
                marginTop: 4,
                padding: "6px 8px",
                borderRadius: 4,
                background: tc.bgCard,
                border: `1px dashed ${tc.warning}80`,
              }}
            >
              <div style={{ fontSize: 10, fontWeight: 700, color: tc.warning, marginBottom: 4 }}>
                {labelByCategory[category]} · Azioni suggerite
              </div>
              <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                {category === "unreachable" && altHosts.length > 0 && (
                  <>
                    <div style={{ fontSize: 9, color: tc.textMuted }}>
                      {t("project-db.cercaIlDbSu")}
                    </div>
                    {altHosts.map((h) => (
                      <button
                        key={h}
                        type="button"
                        disabled={busy}
                        onClick={() => void tryAlternativeHost(h)}
                        style={{
                          alignSelf: "flex-start",
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
                        Prova host {h}
                      </button>
                    ))}
                  </>
                )}
                {category === "no_database" && (
                  <>
                    <div style={{ fontSize: 9, color: tc.textMuted }}>
                      {t("project-db.ilServerERaggiungibile")} <code>{dbName}</code>.
                    </div>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => {
                        const prompt =
                          `Crea il database "${dbName}" sul server PostgreSQL accessibile da ` +
                          `${detectedConfig?.connection_string?.replace(/:([^:@]+)@/, ":***@") ?? "questa configurazione"}, ` +
                          `quindi applica le migrazioni del progetto (cartella migrations) ` +
                          `e verifica con un test di connessione.`;
                        try {
                          window.dispatchEvent(
                            new CustomEvent("nexus:chat:send", { detail: { content: prompt } }),
                          );
                          setActionMsg(`Prompt inviato all'agente per creare "${dbName}".`);
                        } catch {
                          navigator.clipboard?.writeText(prompt).catch(() => {});
                          setActionMsg("Prompt copiato negli appunti. Incollalo in chat.");
                        }
                      }}
                      style={{
                        alignSelf: "flex-start",
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
                      Crea database "{dbName}" via agente
                    </button>
                  </>
                )}
                {category === "tables_missing" && (
                  <>
                    <div style={{ fontSize: 9, color: tc.textMuted }}>
                      Connessione OK ma lo schema e' vuoto. Esegui le migrazioni del progetto.
                    </div>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => {
                        setShowInit(true);
                        setActionMsg("Configura connessione e poi clicca \"Esegui migrazioni\" nella sezione Migrazioni.");
                      }}
                      style={{
                        alignSelf: "flex-start",
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
                      {t("project-db.apriPannelloMigrazioni")}
                    </button>
                  </>
                )}
                {category === "auth_failed" && (
                  <div style={{ fontSize: 9, color: tc.textMuted }}>
                    Username/password sbagliati. Aggiorna i campi nella sezione "Usa questa configurazione" e ritesta.
                  </div>
                )}
                {/* Azione comune sempre disponibile */}
                <button
                  type="button"
                  onClick={() => {
                    const parsedX = detectedConfig?.connection_string
                      ? parseConnectionString(detectedConfig.connection_string)
                      : null;
                    setInitForm((f) => ({
                      ...f,
                      engine: detectedConfig?.engine ?? f.engine,
                      hosting_mode: detectedConfig?.hosting_mode ?? f.hosting_mode,
                      migration_tool: detectedConfig?.migration_tool ?? f.migration_tool,
                      migration_path: detectedConfig?.migration_path ?? f.migration_path,
                      connection_string: detectedConfig?.connection_string ?? f.connection_string,
                    }));
                    if (parsedX) {
                      setUseConnFields(true);
                      setConnFields({
                        host: parsedX.host,
                        port: parsedX.port || "5432",
                        database: parsedX.database,
                        username: parsedX.username,
                        password: parsedX.password,
                      });
                    }
                    setShowInit(true);
                  }}
                  style={{
                    alignSelf: "flex-start",
                    padding: "2px 8px",
                    borderRadius: 4,
                    border: `1px solid ${tc.border}`,
                    background: "transparent",
                    color: tc.textSecondary,
                    cursor: "pointer",
                    fontSize: 10,
                    fontWeight: 600,
                  }}
                >
                  {t("project-db.configuraManualmente")}
                </button>
              </div>
            </div>
          );
        })()}
      </div>
    </div>
  );
}
