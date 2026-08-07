"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import {
  useProjectStore,
  selectDbConfigUpdatedAt,
  selectMigrationsChangedAt,
} from "../../lib/project-dispatcher/store";
import {
  listProjectMigrations,
  applyProjectMigrations,
  rollbackProjectMigration,
  importProjectDbSchema,
  getProjectDbConfig,
  setProjectDbConfig,
  requestProjectDbOverride,
  detectProjectDb,
  provisionProjectDb,
  testProjectDbConnection,
  listProjectDbConnections,
  setPrimaryProjectDbConnection,
  deleteProjectDbConnection,
  type ProjectMigration,
  type ProjectDbConfig,
  type ProjectDbTestResult,
  type ProjectDbConnection,
  type UserProjectDetails,
} from "../../lib/api-client";
import { useGlobalDialog } from "../global-dialog-provider";
import {
  statusColorMap,
  parseConnectionString,
  type ConnFields,
  type DetectedConfig,
} from "./db-helpers";
import { CreateDbWizard } from "./create-db-wizard";
import { ConnectionForm } from "./connection-form";
import { DetectConfig } from "./detect-config";
import { ConnectionList } from "./connection-list";
import { MigrationsSection } from "./migrations-section";
import { RecentQueriesSection } from "./recent-queries-section";
import { useI18n } from "../../lib/i18n";

interface Props {
  project: UserProjectDetails | null;
}

export function ProjectDbPanel({ project }: Props) {
  const { t } = useI18n();
  const tc = useThemeColors();
  const { confirmDialog } = useGlobalDialog();
  const projectId = project?.id ?? "";

  const [migrations, setMigrations] = useState<ProjectMigration[]>([]);
  const [config, setConfig] = useState<ProjectDbConfig | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [actionMsg, setActionMsg] = useState<string | null>(null);
  // Stato per "Importa schema dai file": lista candidati quando ambiguo.
  const [schemaCandidates, setSchemaCandidates] = useState<string[]>([]);
  const [selectedSchemaFile, setSelectedSchemaFile] = useState<string>("");
  const [showInit, setShowInit] = useState(false);
  const [showNewMig, setShowNewMig] = useState(false);
  const [initForm, setInitForm] = useState({
    name: "primary",
    engine: "postgres",
    hosting_mode: "external",
    migration_tool: "generic_sql",
    migration_path: "migrations",
    allow_ddl_override: true,
    connection_string: "",
  });
  // Campi separati per costruire la connection string (piu' intuitivo)
  // Default sensati per il setup locale (host=localhost, porta postgres standard)
  // così il pulsante "Testa connessione" non esplode su placeholder letterali.
  const [connFields, setConnFields] = useState({
    host: "localhost",
    port: "5432",
    database: "",
    username: "",
    password: "",
  });
  const [useConnFields, setUseConnFields] = useState(true);

  // Costruisce la connection string dai campi separati
  const buildConnectionString = () => {
    const { host, port, database, username, password } = connFields;
    if (!host && !database && !username) return "";
    const engine = initForm.engine;
    if (engine === "sqlite") return database; // per sqlite e' solo il path
    if (engine === "sqlserver") {
      return `Server=${host}${port && port !== "1433" ? `,${port}` : ""};Database=${database};User Id=${username};Password=${password};TrustServerCertificate=True;`;
    }
    if (engine === "mysql") {
      return `mysql://${username}:${password}@${host}:${port || "3306"}/${database}`;
    }
    // postgres (default): formato ADO.NET che il backend normalizza
    return `Host=${host};Port=${port || "5432"};Database=${database};Username=${username};Password=${password}`;
  };

  // Restituisce la connection string effettiva (dai campi o dal campo raw)
  const getEffectiveConnectionString = () => {
    if (useConnFields) return buildConnectionString();
    return initForm.connection_string;
  };

  const [migForm, setMigForm] = useState({ sql: "", reason: "" });
  const [detectHints, setDetectHints] = useState<string[] | null>(null);
  const [testResult, setTestResult] = useState<ProjectDbTestResult | null>(null);
  const [connTestResults, setConnTestResults] = useState<Record<string, ProjectDbTestResult>>({});
  const [connTestingId, setConnTestingId] = useState<string | null>(null);
  const [connections, setConnections] = useState<ProjectDbConnection[]>([]);
  const [detectedConfig, setDetectedConfig] = useState<DetectedConfig | null>(null);
  const [detectedTestResult, setDetectedTestResult] = useState<ProjectDbTestResult | null>(null);

  // Wizard "Crea database": guida l utente su DOVE (internal/external) e COME (nome/engine).
  const [showProvision, setShowProvision] = useState(false);
  const [provStep, setProvStep] = useState<"where" | "how">("where");
  const [provMode, setProvMode] = useState<"internal" | "external">("internal");
  const [provName, setProvName] = useState("primary");
  const [provDbName, setProvDbName] = useState("");
  const [provEngine, setProvEngine] = useState("postgres");
  const [provExt, setProvExt] = useState<ConnFields>({ host: "localhost", port: "5432", database: "", username: "", password: "" });
  const [provBusy, setProvBusy] = useState(false);
  const [provResult, setProvResult] = useState<{ ok: boolean; message: string } | null>(null);

  // Nome database fisico suggerito dallo slug del progetto.
  const slugSuggestion = (() => {
    const base = (project?.slug || project?.id || "").toString().toLowerCase();
    const cleaned = base.replace(/[^a-z0-9_]/g, "_").slice(0, 56);
    const safe = /^[0-9]/.test(cleaned) || cleaned === "" ? "p" + cleaned : cleaned;
    return safe ? safe + "_app" : "";
  })();

  const openProvisionWizard = () => {
    setProvStep("where");
    setProvMode("internal");
    setProvName("primary");
    setProvDbName(slugSuggestion);
    setProvEngine("postgres");
    setProvExt({ host: "localhost", port: "5432", database: slugSuggestion, username: "", password: "" });
    setProvResult(null);
    setShowProvision(true);
  };

  const buildExternalConnString = () => {
    const { host, port, database, username, password } = provExt;
    if (provEngine === "sqlite") return database;
    if (provEngine === "mysql") return `mysql://${username}:${password}@${host}:${port || "3306"}/${database}`;
    if (provEngine === "sqlserver") {
      return `Server=${host}${port && port !== "1433" ? `,${port}` : ""};Database=${database};User Id=${username};Password=${password};TrustServerCertificate=True;`;
    }
    return `Host=${host};Port=${port || "5432"};Database=${database};Username=${username};Password=${password}`;
  };

  const handleProvision = async () => {
    if (!projectId) return;
    setProvBusy(true);
    setProvResult(null);
    try {
      const body =
        provMode === "internal"
          ? { mode: "internal" as const, name: provName.trim() || "primary", db_name: provDbName.trim() || undefined, engine: "postgres" }
          : {
              mode: "external" as const,
              name: provName.trim() || "primary",
              engine: provEngine,
              connection_string: buildExternalConnString(),
            };
      const res = await provisionProjectDb(projectId, body);
      if (res.ok) {
        const detail =
          provMode === "internal"
            ? `Database creato: ${res.db_name ?? ""}${res.dsn ? ` (${res.dsn})` : ""}`
            : `Connessione esterna registrata${res.server_version ? ` - ${res.server_version}` : ""}`;
        setProvResult({ ok: true, message: detail });
        await load();
        setTimeout(() => setShowProvision(false), 1200);
      } else {
        setProvResult({ ok: false, message: res.error || "Provisioning fallito" });
      }
    } catch (e) {
      setProvResult({ ok: false, message: e instanceof Error ? e.message : "Errore provisioning" });
    } finally {
      setProvBusy(false);
    }
  };

  const load = useCallback(async () => {
    if (!projectId) {
      setMigrations([]);
      setConfig(null);
      setDetectedConfig(null);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const [mig, cfg, conns, detected] = await Promise.all([
        listProjectMigrations(projectId),
        getProjectDbConfig(projectId).catch(() => null),
        listProjectDbConnections(projectId).catch(() => ({ connections: [] })),
        detectProjectDb(projectId).catch(() => null),
      ]);
      setMigrations(mig.migrations ?? []);
      setConfig(cfg);
      setConnections(conns.connections ?? []);
      if (detected && (detected.engine || (detected.hints?.length ?? 0) > 0)) {
        setDetectedConfig({
          engine: detected.engine ?? undefined,
          hosting_mode: detected.hosting_mode ?? undefined,
          migration_tool: detected.migration_tool ?? undefined,
          migration_path: detected.migration_path ?? undefined,
          connection_string: detected.connection_string ?? undefined,
          hints: detected.hints ?? [],
        });
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore caricamento");
    } finally {
      setLoading(false);
    }
  }, [projectId]);

  useEffect(() => {
    void load();
  }, [load]);

  // Auto-refresh quando l'agente aggiorna la config DB via dispatcher SSE
  const dbConfigUpdatedAt = useProjectStore(selectDbConfigUpdatedAt);
  useEffect(() => {
    if (dbConfigUpdatedAt > 0) void load();
  }, [dbConfigUpdatedAt, load]);

  // Auto-refresh quando una migrazione viene applicata/rollback via dispatcher SSE
  const migrationsChangedAt = useProjectStore(selectMigrationsChangedAt);
  useEffect(() => {
    if (migrationsChangedAt > 0) void load();
  }, [migrationsChangedAt, load]);

  // Auto-test della config rilevata, best-effort. Risolve il caso in cui il DB
  // applicativo ESISTE ed e' raggiungibile, ma non e' registrato in
  // `project_database_config`: prima della UI mostrava solo "Database progetto
  // non configurato" e l'utente non poteva distinguere fra "non esiste" e "non
  // lo vede". Ora il test gira automaticamente quando viene rilevata una
  // connection_string nei sorgenti del progetto e non c'e' ancora una config
  // registrata; il risultato (raggiungibile/non raggiungibile + n. tabelle)
  // viene mostrato dal pannello DetectConfig accanto al bottone "Usa questa
  // configurazione". Idempotente: parte una sola volta per ogni connection
  // string osservata (skip se gia' testata o se test in corso).
  const lastAutoTestedConnRef = useRef<string | null>(null);
  useEffect(() => {
    const cs = detectedConfig?.connection_string?.trim();
    if (
      !cs ||
      !!config ||
      busy ||
      detectedTestResult !== null ||
      lastAutoTestedConnRef.current === cs
    ) {
      return;
    }
    lastAutoTestedConnRef.current = cs;
    void handleTestDetected();
    // handleTestDetected definita sopra; le sue dipendenze (projectId,
    // detectedConfig) sono catturate via closure stabile.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detectedConfig?.connection_string, config, busy, detectedTestResult]);

  const pending = migrations.filter((m) => m.status === "pending");
  const applied = migrations.filter((m) => m.status === "applied");

  const handleApply = async () => {
    if (!projectId) return;
    setBusy(true);
    setActionMsg(null);
    setError(null);
    try {
      const res = await applyProjectMigrations(projectId);
      if (res.ok) {
        setActionMsg(`Applicate ${res.applied?.length ?? 0} migrazioni.`);
        await load();
      } else {
        setError("Applicazione fallita.");
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore");
    } finally {
      setBusy(false);
    }
  };

  const handleRollback = async () => {
    if (!projectId) return;
    setBusy(true);
    setActionMsg(null);
    setError(null);
    try {
      const res = await rollbackProjectMigration(projectId);
      if (res.ok) {
        setActionMsg(`Rollback: ${res.rolled_back ?? ""}`);
        await load();
      } else {
        setError("Rollback fallito.");
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore rollback");
    } finally {
      setBusy(false);
    }
  };

  // Importa lo schema da un file SQL del progetto. Se il backend ritorna piu'
  // candidati (ambiguo) mostra un select per scegliere; alla scelta riapplica
  // con il file selezionato.
  const handleImportSchema = async (filePath?: string) => {
    if (!projectId) return;
    setBusy(true);
    setActionMsg(null);
    setError(null);
    try {
      const res = await importProjectDbSchema(projectId, filePath);
      if (res.ambiguous && res.candidates && res.candidates.length > 0) {
        setSchemaCandidates(res.candidates);
        setSelectedSchemaFile(res.candidates[0]);
        setActionMsg("Piu' file schema trovati: seleziona quello da importare.");
        return;
      }
      if (res.ok) {
        setSchemaCandidates([]);
        setSelectedSchemaFile("");
        setActionMsg(
          `Schema importato da ${res.file ?? "file"} (${res.statements_run ?? 0} statement, ${res.tables_after ?? "?"} tabelle).`
        );
        await load();
      } else {
        setError(res.message ?? "Importazione schema fallita.");
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore importazione schema");
    } finally {
      setBusy(false);
    }
  };

  const isConfigured = !!config?.engine;

  const primaryConn = connections.find((c) => c.is_primary) ?? connections[0] ?? null;

  const handleEditConfig = (conn?: ProjectDbConnection | null) => {
    const source = conn ?? primaryConn;
    setInitForm((f) => ({
      ...f,
      name: source?.name ?? "primary",
      engine: source?.engine ?? config?.engine ?? f.engine,
      hosting_mode: source?.hosting_mode ?? config?.hosting_mode ?? f.hosting_mode,
      migration_tool: source?.migration_tool ?? config?.migration_tool ?? f.migration_tool,
      migration_path: source?.migration_path ?? config?.migration_path ?? f.migration_path,
      allow_ddl_override: source?.allow_ddl_override ?? config?.allow_ddl_override ?? f.allow_ddl_override,
      connection_string: "",
    }));
    // Reset campi separati per nuova immissione (oppure precompila se abbiamo una connessione rilevata)
    const detected = (detectedConfig?.connection_string ?? "").trim();
    const parsed = detected ? parseConnectionString(detected) : null;
    setConnFields(
      parsed
        ? {
            host: parsed.host,
            port: parsed.port || "5432",
            database: parsed.database,
            username: parsed.username,
            password: parsed.password, // solo se presente nella stringa (es. .env); altrimenti resta vuota
          }
        : { host: "", port: "5432", database: "", username: "", password: "" }
    );
    setUseConnFields(true);
    setTestResult(null);
    setDetectHints(null);
    setShowInit(true);
  };

  const handleAddConnection = () => {
    setInitForm({
      name: "",
      engine: "postgres",
      hosting_mode: "external",
      migration_tool: "generic_sql",
      migration_path: "migrations",
      allow_ddl_override: true,
      connection_string: "",
    });
    setTestResult(null);
    setDetectHints(null);
    setShowInit(true);
  };

  const handleSetPrimary = async (connId: string) => {
    if (!projectId) return;
    setBusy(true);
    setError(null);
    try {
      await setPrimaryProjectDbConnection(projectId, connId);
      setActionMsg("Connessione primaria aggiornata.");
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore set-primary");
    } finally {
      setBusy(false);
    }
  };

  const handleTestSavedConnection = async (conn: ProjectDbConnection) => {
    if (!projectId) return;
    setConnTestingId(conn.id);
    setError(null);
    try {
      const res = await testProjectDbConnection(projectId, {
        connection_id: conn.id,
        engine: conn.engine ?? undefined,
      });
      setConnTestResults((r) => ({ ...r, [conn.id]: res }));
    } catch (e) {
      setConnTestResults((r) => ({
        ...r,
        [conn.id]: { ok: false, error: e instanceof Error ? e.message : "Errore test" },
      }));
    } finally {
      setConnTestingId(null);
    }
  };

  const handleDeleteConnection = async (conn: ProjectDbConnection) => {
    if (!projectId) return;
    const ok = await confirmDialog(
      `Eliminare la connessione "${conn.name}"? La configurazione verra' rimossa (le migrazioni restano).`,
      "Elimina connessione DB",
    );
    if (!ok) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await deleteProjectDbConnection(projectId, conn.id);
      setActionMsg(`Connessione "${conn.name}" eliminata.`);
      await load();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore eliminazione");
    } finally {
      setBusy(false);
    }
  };

  const handleDetect = async () => {
    if (!projectId) return;
    setBusy(true);
    setError(null);
    setActionMsg(null);
    setDetectHints(null);
    setDetectedTestResult(null);
    try {
      const res = await detectProjectDb(projectId);
      setInitForm((f) => ({
        ...f,
        engine: res.engine ?? f.engine,
        hosting_mode: res.hosting_mode ?? f.hosting_mode,
        migration_tool: res.migration_tool ?? f.migration_tool,
        migration_path: res.migration_path ?? f.migration_path,
        connection_string: res.connection_string ?? f.connection_string,
      }));
      const detectedConn = (res.connection_string ?? "").trim();
      const parsed = detectedConn ? parseConnectionString(detectedConn) : null;
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
      setDetectHints(res.hints ?? []);
      setShowInit(true);
      setActionMsg(
        (res.hints?.length ?? 0) > 0
          ? `Rilevato: ${(res.hints ?? []).slice(0, 3).join(", ")}`
          : "Nessun indicatore DB trovato nei sorgenti."
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore rilevamento");
    } finally {
      setBusy(false);
    }
  };

  const handleTestDetected = async () => {
    if (!projectId) return;
    const connStr = (detectedConfig?.connection_string ?? "").trim();
    if (!connStr) {
      setDetectedTestResult({ ok: false, error: "Nessuna connection string rilevata." });
      return;
    }
    setBusy(true);
    setError(null);
    setDetectedTestResult(null);
    try {
      const res = await testProjectDbConnection(projectId, {
        engine: detectedConfig?.engine ?? undefined,
        connection_string: connStr,
      });
      setDetectedTestResult(res);
    } catch (e) {
      setDetectedTestResult({ ok: false, error: e instanceof Error ? e.message : "Errore test" });
    } finally {
      setBusy(false);
    }
  };

  const handleTestConnection = async () => {
    if (!projectId) return;
    // Validazione client: evitiamo che placeholder/empty arrivino al backend
    // generando un errore criptico "failed to lookup address information".
    if (useConnFields && initForm.engine !== "sqlite") {
      const { host, database, username } = connFields;
      const missing: string[] = [];
      if (!host.trim()) missing.push("Host");
      if (!database.trim()) missing.push("Database");
      if (!username.trim()) missing.push("Utente");
      if (missing.length > 0) {
        setTestResult({ ok: false, error: `Campi obbligatori vuoti: ${missing.join(", ")}` });
        return;
      }
    } else if (!useConnFields) {
      if (!(initForm.connection_string ?? "").trim()) {
        setTestResult({ ok: false, error: "Connection string vuota." });
        return;
      }
    }
    setBusy(true);
    setError(null);
    setTestResult(null);
    try {
      const connStr = getEffectiveConnectionString();
      const res = await testProjectDbConnection(projectId, {
        engine: initForm.engine,
        connection_string: connStr || undefined,
      });
      setTestResult(res);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore test");
    } finally {
      setBusy(false);
    }
  };

  const handleInit = async () => {
    if (!projectId) return;
    setBusy(true);
    setError(null);
    setActionMsg(null);
    try {
      const connStr = getEffectiveConnectionString();
      const res = await setProjectDbConfig(projectId, {
        name: initForm.name || "primary",
        engine: initForm.engine,
        hosting_mode: initForm.hosting_mode,
        migration_tool: initForm.migration_tool,
        migration_path: initForm.migration_path,
        allow_ddl_override: initForm.allow_ddl_override,
        connection_string: connStr || undefined,
      });
      if (res.ok) {
        setActionMsg("Database progetto inizializzato.");
        setShowInit(false);
        await load();
      } else {
        setError("Inizializzazione fallita.");
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore init");
    } finally {
      setBusy(false);
    }
  };

  const handleCreateMigration = async () => {
    if (!projectId) return;
    if (!migForm.sql.trim()) {
      setError("SQL obbligatorio.");
      return;
    }
    if (migForm.reason.trim().length < 10) {
      setError("Motivo: almeno 10 caratteri.");
      return;
    }
    setBusy(true);
    setError(null);
    setActionMsg(null);
    try {
      const res = await requestProjectDbOverride(projectId, migForm.sql, migForm.reason);
      if (res.ok) {
        setActionMsg(`Migrazione creata: ${res.filename ?? ""}`);
        setMigForm({ sql: "", reason: "" });
        setShowNewMig(false);
        await load();
      } else {
        setError("Creazione migrazione fallita.");
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore creazione");
    } finally {
      setBusy(false);
    }
  };

  const statusColor = (s: string) => statusColorMap[s] ?? tc.textMuted;

  if (!project) {
    return (
      <div style={{ padding: 16, color: tc.textMuted, fontSize: 12 }}>
        {t("project-db.apriUnProgettoPer")}
      </div>
    );
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%", minHeight: 0 }}>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "6px 10px",
          borderBottom: `1px solid ${tc.border}`,
          background: tc.bgSidebar,
        }}
      >
        <div>
          <div
            style={{
              fontSize: 12,
              fontWeight: 700,
              color: tc.text,
              textTransform: "uppercase",
              letterSpacing: "0.06em",
            }}
          >
            {t("project-db.databaseProgetto")}
          </div>
          <div style={{ fontSize: 11, color: tc.textMuted, marginTop: 2 }}>
            {project.name ?? projectId}
          </div>
        </div>
        <button
          type="button"
          onClick={() => void load()}
          title={t("project-db.aggiorna")}
          aria-label={t("project-db.aggiorna")}
          style={{
            width: 28,
            height: 28,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
            color: tc.textSecondary,
            borderRadius: 6,
            cursor: "pointer",
            fontSize: 13,
          }}
        >
          ↻
        </button>
      </div>

      <div style={{ flex: 1, minHeight: 0, overflow: "auto" }}>
        {showProvision && (
          <CreateDbWizard
            tc={tc}
            provStep={provStep}
            setProvStep={setProvStep}
            provMode={provMode}
            setProvMode={setProvMode}
            provName={provName}
            setProvName={setProvName}
            provDbName={provDbName}
            setProvDbName={setProvDbName}
            provEngine={provEngine}
            setProvEngine={setProvEngine}
            provExt={provExt}
            setProvExt={setProvExt}
            provBusy={provBusy}
            provResult={provResult}
            slugSuggestion={slugSuggestion}
            onClose={() => setShowProvision(false)}
            onProvision={() => void handleProvision()}
          />
        )}

        {!isConfigured && !showInit && !loading && (
          <div style={{ padding: 10, borderBottom: `1px solid ${tc.border}`, background: `${tc.accent}10` }}>
            <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
              <div style={{ fontSize: 11, color: tc.textSecondary }}>
                Database progetto non configurato. Crea il database scegliendo dove e come, oppure rileva la configurazione dai sorgenti.
              </div>
              <button
                type="button"
                onClick={openProvisionWizard}
                style={{
                  padding: "8px 10px",
                  borderRadius: 6,
                  border: "none",
                  background: tc.accent,
                  color: "#fff",
                  cursor: "pointer",
                  fontSize: 12,
                  fontWeight: 700,
                }}
              >
                {t("project-db.creaDatabase")}
              </button>
              <button
                type="button"
                onClick={() => void handleDetect()}
                disabled={busy}
                style={{
                  padding: "6px 8px",
                  borderRadius: 6,
                  border: `1px solid ${tc.accent}`,
                  background: "transparent",
                  color: tc.accent,
                  cursor: busy ? "not-allowed" : "pointer",
                  fontSize: 12,
                  fontWeight: 600,
                }}
              >
                {busy ? "Rilevamento…" : "Rileva dai sorgenti"}
              </button>
              <button
                type="button"
                onClick={() => setShowInit(true)}
                style={{
                  padding: "6px 8px",
                  borderRadius: 6,
                  border: "none",
                  background: tc.accent,
                  color: "#fff",
                  cursor: "pointer",
                  fontSize: 12,
                  fontWeight: 600,
                }}
              >
                {t("project-db.configuraManualmente")}
              </button>
            </div>
          </div>
        )}

        {showInit && (
          <ConnectionForm
            tc={tc}
            isConfigured={isConfigured}
            initForm={initForm}
            setInitForm={setInitForm}
            connFields={connFields}
            setConnFields={setConnFields}
            useConnFields={useConnFields}
            setUseConnFields={setUseConnFields}
            busy={busy}
            testResult={testResult}
            detectHints={detectHints}
            buildConnectionString={buildConnectionString}
            onTestConnection={() => void handleTestConnection()}
            onInit={() => void handleInit()}
            onCancel={() => setShowInit(false)}
          />
        )}

        {detectedConfig && !showInit && (
          <DetectConfig
            tc={tc}
            projectId={projectId}
            detectedConfig={detectedConfig}
            detectedTestResult={detectedTestResult}
            loading={loading}
            busy={busy}
            setBusy={setBusy}
            setDetectedConfig={setDetectedConfig}
            setDetectedTestResult={setDetectedTestResult}
            setInitForm={setInitForm}
            setUseConnFields={setUseConnFields}
            setConnFields={setConnFields}
            setDetectHints={setDetectHints}
            setTestResult={setTestResult}
            setShowInit={setShowInit}
            setActionMsg={setActionMsg}
            onReload={() => void load()}
            onTestDetected={() => void handleTestDetected()}
          />
        )}

        {isConfigured && !showInit && (
          <ConnectionList
            tc={tc}
            connections={connections}
            config={config}
            busy={busy}
            showNewMig={showNewMig}
            connTestingId={connTestingId}
            connTestResults={connTestResults}
            onProvisionWizard={openProvisionWizard}
            onAddConnection={handleAddConnection}
            onTestSaved={(conn) => void handleTestSavedConnection(conn)}
            onSetPrimary={(connId) => void handleSetPrimary(connId)}
            onEditConfig={(conn) => handleEditConfig(conn)}
            onDeleteConnection={(conn) => void handleDeleteConnection(conn)}
            onToggleNewMig={() => setShowNewMig((v) => !v)}
          />
        )}

        <MigrationsSection
          tc={tc}
          showNewMig={showNewMig}
          isConfigured={isConfigured}
          migForm={migForm}
          setMigForm={setMigForm}
          busy={busy}
          loading={loading}
          error={error}
          actionMsg={actionMsg}
          migrations={migrations}
          pending={pending}
          applied={applied}
          schemaCandidates={schemaCandidates}
          selectedSchemaFile={selectedSchemaFile}
          setSelectedSchemaFile={setSelectedSchemaFile}
          statusColor={statusColor}
          onCreateMigration={() => void handleCreateMigration()}
          onApply={() => void handleApply()}
          onRollback={() => void handleRollback()}
          onImportSchema={(filePath) => void handleImportSchema(filePath)}
        />

        {/* ── Query recenti (event-driven via dispatcher) ─────────────────── */}
        <RecentQueriesSection />
      </div>
    </div>
  );
}
