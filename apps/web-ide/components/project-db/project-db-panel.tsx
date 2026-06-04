"use client";

import { useCallback, useEffect, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import {
  useProjectStore,
  selectDbConfigUpdatedAt,
  selectMigrationsChangedAt,
  selectDatabaseQueries,
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

interface Props {
  project: UserProjectDetails | null;
}

const statusColorMap: Record<string, string> = {
  applied: "#22c55e",
  pending: "#f59e0b",
  failed: "#ef4444",
  rolled_back: "#6b7280",
  overridden: "#8b5cf6",
};

/** Classifica l'errore di test-connection per proporre azioni mirate.
 *  - `unreachable`: host/porta non raggiungibili (connection refused, timeout, DNS).
 *  - `no_database` : il database non esiste sul server (es. "database X does not exist").
 *  - `auth_failed` : credenziali sbagliate (password authentication failed, role does not exist).
 *  - `tables_missing`: connessione OK ma schema vuoto (table_count=0 senza migrazioni applicate).
 *  - `unknown`     : tutto il resto (errore SQL generico, sintassi, permessi su singolo oggetto).
 */
type DbErrorCategory = "unreachable" | "no_database" | "auth_failed" | "tables_missing" | "unknown";
function categorizeDbError(error: string | null | undefined, tableCount?: number | null): DbErrorCategory {
  if ((tableCount ?? -1) === 0) return "tables_missing";
  if (!error) return "unknown";
  const lower = error.toLowerCase();
  if (
    lower.includes("connection refused") ||
    lower.includes("connessione rifiutata") ||
    lower.includes("no route to host") ||
    lower.includes("network is unreachable") ||
    lower.includes("lookup address information") ||
    lower.includes("name or service not known") ||
    lower.includes("could not connect") ||
    lower.includes("timed out") ||
    lower.includes("timeout") ||
    lower.includes("server closed the connection")
  ) return "unreachable";
  if (
    lower.includes("does not exist") && (lower.includes("database") || lower.includes("3d000")) ||
    lower.includes("unknown database") ||
    lower.includes("no such database")
  ) return "no_database";
  if (
    lower.includes("password authentication failed") ||
    lower.includes("authentication failed") ||
    lower.includes("role") && lower.includes("does not exist") ||
    lower.includes("access denied") ||
    lower.includes("28p01") ||
    lower.includes("28000")
  ) return "auth_failed";
  return "unknown";
}

/** Estrae host/port/db/user da una connection string semplificata. */
function parseConnPartsForActions(connStr: string): { host: string; port: string; database: string } | null {
  // postgres://user:pass@host:port/dbname o postgresql:// ; mysql:// ; mssql:// ; ecc.
  const m = connStr.match(/^[a-z]+:\/\/(?:[^@]+@)?([^:/]+)(?::(\d+))?(?:\/([^?]+))?/i);
  if (!m) return null;
  return { host: m[1] ?? "", port: m[2] ?? "", database: (m[3] ?? "").split("?")[0] ?? "" };
}

/** Lista di host candidati quando il primo è unreachable (es. localhost dev → server prod). */
function alternativeHostsFor(currentHost: string): string[] {
  const local = ["localhost", "127.0.0.1", "::1"];
  if (local.includes(currentHost)) return ["192.168.0.6", "192.168.0.3"];
  if (currentHost === "192.168.0.6") return ["192.168.0.3", "localhost"];
  if (currentHost === "192.168.0.3") return ["192.168.0.6", "localhost"];
  return ["localhost", "192.168.0.6", "192.168.0.3"].filter((h) => h !== currentHost);
}

import { useGlobalDialog } from "../global-dialog-provider";

export function ProjectDbPanel({ project }: Props) {
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
  const [detectedConfig, setDetectedConfig] = useState<{
    engine?: string;
    hosting_mode?: string;
    migration_tool?: string;
    migration_path?: string;
    connection_string?: string;
    hints: string[];
  } | null>(null);
  const [detectedTestResult, setDetectedTestResult] = useState<ProjectDbTestResult | null>(null);

  // Wizard "Crea database": guida l utente su DOVE (internal/external) e COME (nome/engine).
  const [showProvision, setShowProvision] = useState(false);
  const [provStep, setProvStep] = useState<"where" | "how">("where");
  const [provMode, setProvMode] = useState<"internal" | "external">("internal");
  const [provName, setProvName] = useState("primary");
  const [provDbName, setProvDbName] = useState("");
  const [provEngine, setProvEngine] = useState("postgres");
  const [provExt, setProvExt] = useState({ host: "localhost", port: "5432", database: "", username: "", password: "" });
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

  // Parser per estrarre host/port/db/user/pass da una connection string
  const parseConnectionString = (raw: string): { host: string; port: string; database: string; username: string; password: string } | null => {
    const trimmed = raw.trim();
    // Formato URL: postgres://user:pass@host:port/db
    const urlMatch = trimmed.match(/^(?:postgres(?:ql)?|mysql):\/\/([^:]+):([^@]*)@([^:/?]+):?(\d+)?\/(.+)$/);
    if (urlMatch) {
      return { username: urlMatch[1], password: decodeURIComponent(urlMatch[2]), host: urlMatch[3], port: urlMatch[4] || "5432", database: urlMatch[5] };
    }
    // Formato ADO.NET: Host=...;Port=...;...
    if (/[Hh]ost=|[Ss]erver=|[Dd]atabase=/.test(trimmed)) {
      const get = (key: string) => {
        const m = trimmed.match(new RegExp(`${key}\\s*=\\s*([^;]*)`, "i"));
        return m ? m[1].trim() : "";
      };
      const host = get("Host") || get("Server") || get("Data Source");
      const port = get("Port") || "5432";
      const database = get("Database") || get("Initial Catalog");
      const username = get("Username") || get("User Id") || get("User");
      const password = get("Password");
      if (host || database) return { host, port, database, username, password };
    }
    return null;
  };

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
        Apri un progetto per gestirne il database.
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
            Database progetto
          </div>
          <div style={{ fontSize: 11, color: tc.textMuted, marginTop: 2 }}>
            {project.name ?? projectId}
          </div>
        </div>
        <button
          type="button"
          onClick={() => void load()}
          title="Aggiorna"
          aria-label="Aggiorna"
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
        <div
          style={{
            position: "fixed",
            inset: 0,
            zIndex: 1000,
            background: "rgba(0,0,0,0.55)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            padding: 16,
          }}
          onClick={() => { if (!provBusy) setShowProvision(false); }}
        >
          <div
            onClick={(e) => e.stopPropagation()}
            style={{
              width: 480,
              maxWidth: "100%",
              maxHeight: "90vh",
              overflow: "auto",
              background: tc.bgCard,
              border: `1px solid ${tc.border}`,
              borderRadius: 10,
              padding: 16,
              display: "flex",
              flexDirection: "column",
              gap: 12,
            }}
          >
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
              <div style={{ fontSize: 14, fontWeight: 700, color: tc.text }}>Crea database</div>
              <button
                type="button"
                onClick={() => { if (!provBusy) setShowProvision(false); }}
                style={{ border: "none", background: "transparent", color: tc.textMuted, cursor: "pointer", fontSize: 16, lineHeight: 1 }}
              >
                x
              </button>
            </div>

            <div style={{ display: "flex", gap: 8, fontSize: 11 }}>
              <span style={{ color: provStep === "where" ? tc.accent : tc.textMuted, fontWeight: 700 }}>1. Dove</span>
              <span style={{ color: tc.textMuted }}>-</span>
              <span style={{ color: provStep === "how" ? tc.accent : tc.textMuted, fontWeight: 700 }}>2. Come</span>
            </div>

            {provStep === "where" && (
              <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
                <button
                  type="button"
                  onClick={() => { setProvMode("internal"); setProvEngine("postgres"); }}
                  style={{
                    textAlign: "left",
                    padding: 12,
                    borderRadius: 8,
                    border: `2px solid ${provMode === "internal" ? tc.accent : tc.border}`,
                    background: provMode === "internal" ? `${tc.accent}12` : "transparent",
                    color: tc.text,
                    cursor: "pointer",
                  }}
                >
                  <div style={{ fontSize: 12, fontWeight: 700 }}>Container dedicato (Nexus)</div>
                  <div style={{ fontSize: 11, color: tc.textSecondary, marginTop: 4 }}>
                    Nexus crea un PostgreSQL isolato per questo progetto nel cluster dedicato.
                    Nessuna credenziale richiesta: il database e separato da quelli degli altri progetti.
                  </div>
                </button>
                <button
                  type="button"
                  onClick={() => setProvMode("external")}
                  style={{
                    textAlign: "left",
                    padding: 12,
                    borderRadius: 8,
                    border: `2px solid ${provMode === "external" ? tc.accent : tc.border}`,
                    background: provMode === "external" ? `${tc.accent}12` : "transparent",
                    color: tc.text,
                    cursor: "pointer",
                  }}
                >
                  <div style={{ fontSize: 12, fontWeight: 700 }}>Database esterno</div>
                  <div style={{ fontSize: 11, color: tc.textSecondary, marginTop: 4 }}>
                    Usa un database gia esistente fornendo host, porta e credenziali.
                    La connessione viene testata prima di essere registrata.
                  </div>
                </button>
              </div>
            )}

            {provStep === "how" && (
              <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
                <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                  <label style={{ fontSize: 10, color: tc.textMuted }}>Nome connessione logica</label>
                  <input
                    type="text"
                    value={provName}
                    placeholder="primary"
                    onChange={(e) => setProvName(e.target.value)}
                    style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
                  />
                </div>

                {provMode === "internal" && (
                  <>
                    <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                      <label style={{ fontSize: 10, color: tc.textMuted }}>Nome del database</label>
                      <input
                        type="text"
                        value={provDbName}
                        placeholder={slugSuggestion}
                        onChange={(e) => setProvDbName(e.target.value)}
                        style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
                      />
                      <div style={{ fontSize: 10, color: tc.textMuted }}>
                        Suggerito dallo slug del progetto. Caratteri non validi vengono sostituiti con underscore.
                      </div>
                    </div>
                    <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                      <label style={{ fontSize: 10, color: tc.textMuted }}>Engine</label>
                      <select
                        value="postgres"
                        disabled
                        style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
                      >
                        <option value="postgres">PostgreSQL</option>
                      </select>
                    </div>
                  </>
                )}

                {provMode === "external" && (
                  <>
                    <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                      <label style={{ fontSize: 10, color: tc.textMuted }}>Engine</label>
                      <select
                        value={provEngine}
                        onChange={(e) => setProvEngine(e.target.value)}
                        style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
                      >
                        <option value="postgres">PostgreSQL</option>
                        <option value="mysql">MySQL</option>
                        <option value="sqlserver">SQL Server</option>
                        <option value="sqlite">SQLite</option>
                      </select>
                    </div>
                    {provEngine !== "sqlite" && (
                      <div style={{ display: "flex", gap: 6 }}>
                        <div style={{ flex: 2, display: "flex", flexDirection: "column", gap: 4 }}>
                          <label style={{ fontSize: 10, color: tc.textMuted }}>Host</label>
                          <input type="text" value={provExt.host} onChange={(e) => setProvExt((p) => ({ ...p, host: e.target.value }))}
                            style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }} />
                        </div>
                        <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 4 }}>
                          <label style={{ fontSize: 10, color: tc.textMuted }}>Porta</label>
                          <input type="text" value={provExt.port} onChange={(e) => setProvExt((p) => ({ ...p, port: e.target.value }))}
                            style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }} />
                        </div>
                      </div>
                    )}
                    <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
                      <label style={{ fontSize: 10, color: tc.textMuted }}>{provEngine === "sqlite" ? "Percorso file" : "Nome database"}</label>
                      <input type="text" value={provExt.database} onChange={(e) => setProvExt((p) => ({ ...p, database: e.target.value }))}
                        style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }} />
                    </div>
                    {provEngine !== "sqlite" && (
                      <div style={{ display: "flex", gap: 6 }}>
                        <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 4 }}>
                          <label style={{ fontSize: 10, color: tc.textMuted }}>Utente</label>
                          <input type="text" value={provExt.username} onChange={(e) => setProvExt((p) => ({ ...p, username: e.target.value }))}
                            style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }} />
                        </div>
                        <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 4 }}>
                          <label style={{ fontSize: 10, color: tc.textMuted }}>Password</label>
                          <input type="password" value={provExt.password} onChange={(e) => setProvExt((p) => ({ ...p, password: e.target.value }))}
                            style={{ padding: "5px 7px", fontSize: 11, background: tc.bg, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }} />
                        </div>
                      </div>
                    )}
                  </>
                )}
              </div>
            )}

            {provResult && (
              <div
                style={{
                  fontSize: 11,
                  padding: 8,
                  borderRadius: 6,
                  border: `1px solid ${provResult.ok ? "#22c55e" : "#ef4444"}`,
                  background: provResult.ok ? "#22c55e18" : "#ef444418",
                  color: tc.text,
                  wordBreak: "break-all",
                }}
              >
                {provResult.message}
              </div>
            )}

            <div style={{ display: "flex", justifyContent: "space-between", gap: 8, marginTop: 4 }}>
              <button
                type="button"
                disabled={provBusy}
                onClick={() => {
                  if (provStep === "how") setProvStep("where");
                  else setShowProvision(false);
                }}
                style={{
                  padding: "7px 12px",
                  borderRadius: 6,
                  border: `1px solid ${tc.border}`,
                  background: "transparent",
                  color: tc.text,
                  cursor: provBusy ? "not-allowed" : "pointer",
                  fontSize: 12,
                }}
              >
                {provStep === "how" ? "Indietro" : "Annulla"}
              </button>
              {provStep === "where" ? (
                <button
                  type="button"
                  onClick={() => setProvStep("how")}
                  style={{
                    padding: "7px 14px",
                    borderRadius: 6,
                    border: "none",
                    background: tc.accent,
                    color: "#fff",
                    cursor: "pointer",
                    fontSize: 12,
                    fontWeight: 700,
                  }}
                >
                  Avanti
                </button>
              ) : (
                <button
                  type="button"
                  disabled={provBusy}
                  onClick={() => void handleProvision()}
                  style={{
                    padding: "7px 14px",
                    borderRadius: 6,
                    border: "none",
                    background: tc.accent,
                    color: "#fff",
                    cursor: provBusy ? "not-allowed" : "pointer",
                    fontSize: 12,
                    fontWeight: 700,
                  }}
                >
                  {provBusy ? "Creazione in corso..." : "Crea"}
                </button>
              )}
            </div>
          </div>
        </div>
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
              Crea database
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
              Configura manualmente
            </button>
          </div>
        </div>
      )}

      {showInit && (
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
                onClick={() => void handleTestConnection()}
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
                  onClick={() => void handleInit()}
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
                  onClick={() => setShowInit(false)}
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
      )}

      {/* Sezione "Rilevato dai sorgenti del progetto" — separata dal DB interno di Nexus */}
      {detectedConfig && !showInit && (
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
              Rilevato dai sorgenti del progetto
            </div>
            <button
              type="button"
              onClick={() => void load()}
              title="Ri-analizza i file di config (utile dopo modifiche apportate dalla chat di Nexus)"
              disabled={loading}
              style={{
                padding: "1px 8px", fontSize: 10,
                borderRadius: 3, border: `1px solid ${tc.border}`,
                background: "transparent", color: tc.textMuted,
                cursor: loading ? "default" : "pointer",
                fontFamily: '"JetBrains Mono", monospace',
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
                  <span style={{ color: tc.textSecondary }}>Engine:</span>{" "}
                  <span style={{ color: tc.text, fontWeight: 600 }}>{detectedConfig.engine}</span>
                  {detectedConfig.hosting_mode ? ` · ${detectedConfig.hosting_mode}` : ""}
                </div>
              )}
              {detectedConfig.migration_tool && (
                <div>
                  <span style={{ color: tc.textSecondary }}>Tool:</span> {detectedConfig.migration_tool}
                  {detectedConfig.migration_path ? ` · ${detectedConfig.migration_path}` : ""}
                </div>
              )}
              {detectedConfig.connection_string && (
                <div style={{ fontFamily: "monospace", fontSize: 9, color: tc.textMuted, wordBreak: "break-all", marginTop: 2 }}>
                  <span style={{ color: tc.textSecondary }}>Connessione:</span>{" "}
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
              Usa questa configurazione
            </button>

            <button
              type="button"
              onClick={() => void handleTestDetected()}
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
              title="Esegue un test di connessione usando la config rilevata (senza salvare)."
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
                La categorizzazione e' deterministica (parsing testo errore +
                table_count) e le azioni sono context-aware: per "unreachable"
                tentiamo automaticamente host alternativi sulla LAN; per
                "no_database" generiamo un prompt agente per creare il DB;
                per "tables_missing" suggeriamo di eseguire le migrazioni. */}
            {detectedTestResult && !detectedTestResult.ok && detectedConfig?.connection_string && (() => {
              const category = categorizeDbError(detectedTestResult.error, detectedTestResult.table_count);
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
                          Cerca il DB su altre macchine della LAN:
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
                          Il server e' raggiungibile ma manca il database <code>{dbName}</code>.
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
                          Apri pannello migrazioni
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
                      Configura manualmente
                    </button>
                  </div>
                </div>
              );
            })()}
          </div>
        </div>
      )}

      {isConfigured && !showInit && (
        <div style={{ padding: 10, display: "flex", flexDirection: "column", gap: 6, borderBottom: `1px solid ${tc.border}` }}>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
            <div style={{ fontSize: 11, fontWeight: 600, color: tc.text }}>
              DB del progetto ({connections.length})
            </div>
            <div style={{ display: "flex", gap: 6 }}>
              <button
                type="button"
                onClick={openProvisionWizard}
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
                onClick={handleAddConnection}
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
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", gap: 6 }}>
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
                    onClick={() => void handleTestSavedConnection(c)}
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
                      onClick={() => void handleSetPrimary(c.id)}
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
                    onClick={() => handleEditConfig(c)}
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
                    onClick={() => void handleDeleteConnection(c)}
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
              onClick={() => setShowNewMig((v) => !v)}
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
      )}

      {showNewMig && isConfigured && (
        <div style={{ padding: 10, borderBottom: `1px solid ${tc.border}`, display: "flex", flexDirection: "column", gap: 6 }}>
          <label style={{ fontSize: 10, color: tc.textMuted }}>SQL</label>
          <textarea
            value={migForm.sql}
            onChange={(e) => setMigForm((f) => ({ ...f, sql: e.target.value }))}
            rows={6}
            placeholder="CREATE TABLE ..."
            style={{ padding: 6, fontSize: 11, fontFamily: "monospace", background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4, resize: "vertical" }}
          />
          <label style={{ fontSize: 10, color: tc.textMuted }}>Motivo (min 10 caratteri)</label>
          <input
            type="text"
            value={migForm.reason}
            onChange={(e) => setMigForm((f) => ({ ...f, reason: e.target.value }))}
            style={{ padding: "4px 6px", fontSize: 11, background: tc.bgCard, color: tc.text, border: `1px solid ${tc.border}`, borderRadius: 4 }}
          />
          <button
            type="button"
            onClick={() => void handleCreateMigration()}
            disabled={busy}
            style={{
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
            {busy ? "…" : "Crea ed esegui"}
          </button>
        </div>
      )}

      <div style={{ padding: 10, display: "flex", gap: 6, borderBottom: `1px solid ${tc.border}` }}>
        <button
          type="button"
          onClick={() => void handleApply()}
          disabled={busy || pending.length === 0}
          title="Applica migrazioni pending"
          style={{
            flex: 1,
            padding: "6px 8px",
            borderRadius: 6,
            border: "none",
            background: pending.length > 0 ? tc.accent : tc.bgCard,
            color: pending.length > 0 ? "#fff" : tc.textMuted,
            cursor: pending.length > 0 && !busy ? "pointer" : "not-allowed",
            fontSize: 12,
            fontWeight: 600,
          }}
        >
          {busy ? "…" : `Applica (${pending.length})`}
        </button>
        <button
          type="button"
          onClick={() => void handleRollback()}
          disabled={busy || applied.length === 0}
          title="Rollback ultima"
          style={{
            flex: 1,
            padding: "6px 8px",
            borderRadius: 6,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
            color: applied.length > 0 ? tc.text : tc.textMuted,
            cursor: applied.length > 0 && !busy ? "pointer" : "not-allowed",
            fontSize: 12,
          }}
        >
          Rollback
        </button>
      </div>

      <div style={{ padding: "14px 10px 12px", marginTop: 4, display: "flex", flexDirection: "column", gap: 6, borderBottom: `1px solid ${tc.border}` }}>
        <button
          type="button"
          onClick={() => void handleImportSchema(selectedSchemaFile || undefined)}
          disabled={busy}
          title="Cerca un file schema nel progetto ed eseguilo sul database"
          style={{
            width: "100%",
            padding: "8px 8px",
            borderRadius: 6,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
            color: tc.text,
            cursor: busy ? "not-allowed" : "pointer",
            fontSize: 12,
            fontWeight: 600,
            textAlign: "center",
          }}
        >
          {busy ? "…" : "Importa schema dai file"}
        </button>
        {schemaCandidates.length > 0 && (
          <div style={{ display: "flex", gap: 6 }}>
            <select
              value={selectedSchemaFile}
              onChange={(e) => setSelectedSchemaFile(e.target.value)}
              style={{
                flex: 1,
                padding: "6px 8px",
                borderRadius: 6,
                border: `1px solid ${tc.border}`,
                background: tc.bgCard,
                color: tc.text,
                fontSize: 11,
              }}
            >
              {schemaCandidates.map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
            <button
              type="button"
              onClick={() => void handleImportSchema(selectedSchemaFile)}
              disabled={busy || !selectedSchemaFile}
              style={{
                padding: "6px 10px",
                borderRadius: 6,
                border: "none",
                background: tc.accent,
                color: "#fff",
                cursor: busy ? "not-allowed" : "pointer",
                fontSize: 11,
                fontWeight: 600,
              }}
            >
              Importa
            </button>
          </div>
        )}
      </div>

      <div style={{ flex: 1, minHeight: 0, overflowY: "auto", padding: 10, display: "flex", flexDirection: "column", gap: 6 }}>
        {error && (
          <div
            style={{
              color: tc.error,
              background: `${tc.error}20`,
              border: `1px solid ${tc.error}40`,
              borderRadius: 6,
              padding: "6px 8px",
              fontSize: 11,
            }}
          >
            {error}
          </div>
        )}
        {actionMsg && (
          <div
            style={{
              color: "#22c55e",
              background: "#22c55e20",
              border: "1px solid #22c55e40",
              borderRadius: 6,
              padding: "6px 8px",
              fontSize: 11,
            }}
          >
            {actionMsg}
          </div>
        )}

        {loading ? (
          <div style={{ color: tc.textMuted, fontSize: 12 }}>Caricamento…</div>
        ) : migrations.length === 0 ? (
          <div
            style={{
              color: tc.textMuted,
              fontSize: 12,
              padding: 14,
              textAlign: "center",
              border: `1px dashed ${tc.border}`,
              borderRadius: 6,
            }}
          >
            Nessuna migrazione trovata.
          </div>
        ) : (
          migrations.map((m) => (
            <div
              key={m.id}
              style={{
                background: tc.bgCard,
                border: `1px solid ${tc.border}`,
                borderRadius: 6,
                padding: "8px 10px",
                display: "flex",
                alignItems: "center",
                gap: 8,
              }}
            >
              <span
                style={{
                  width: 8,
                  height: 8,
                  borderRadius: "50%",
                  background: statusColor(m.status),
                  flexShrink: 0,
                }}
              />
              <div style={{ flex: 1, minWidth: 0 }}>
                <div
                  style={{
                    fontSize: 12,
                    fontWeight: 600,
                    color: tc.text,
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                  title={m.filename}
                >
                  {m.filename}
                </div>
                {m.description && (
                  <div
                    style={{
                      fontSize: 11,
                      color: tc.textMuted,
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                    }}
                  >
                    {m.description}
                  </div>
                )}
              </div>
              <span
                style={{
                  fontSize: 10,
                  color: statusColor(m.status),
                  fontWeight: 600,
                  textTransform: "uppercase",
                  letterSpacing: "0.04em",
                  flexShrink: 0,
                }}
              >
                {m.status}
              </span>
            </div>
          ))
        )}
      </div>

      {/* ── Query recenti (event-driven via dispatcher) ─────────────────── */}
      <RecentQueriesSection />
      </div>
    </div>
  );
}

// ── Sub-componente: query recenti dal dispatcher SSE ───────────────────────
// Mostra le ultime DbQueryRun emesse dal tool project_db_query (max 100 in
// store, qui ne renderizziamo 10). Live: niente API call, niente polling.
function RecentQueriesSection() {
  const tc = useThemeColors();
  const queries = useProjectStore(selectDatabaseQueries);
  const [collapsed, setCollapsed] = useState(true);

  if (queries.length === 0) return null;

  const recent = queries.slice(0, 10);
  return (
    <div style={{ marginTop: 16 }}>
      <button
        onClick={() => setCollapsed((c) => !c)}
        style={{
          width: "100%",
          textAlign: "left",
          background: "transparent",
          border: `1px solid ${tc.border}`,
          borderRadius: 6,
          padding: "8px 10px",
          color: tc.text,
          fontSize: 12,
          fontWeight: 600,
          cursor: "pointer",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
        }}
      >
        <span>Query recenti ({queries.length})</span>
        <span style={{ color: tc.textMuted, fontSize: 11 }}>{collapsed ? "▸" : "▾"}</span>
      </button>
      {!collapsed && (
        <div style={{ marginTop: 6, display: "flex", flexDirection: "column", gap: 4 }}>
          {recent.map((q, i) => (
            <div
              key={i}
              style={{
                fontSize: 11,
                color: tc.textMuted,
                padding: "4px 8px",
                borderLeft: `2px solid ${q.kind === "select" ? "#22c55e" : "#f59e0b"}`,
                background: tc.bgCard,
                display: "flex",
                gap: 8,
                alignItems: "center",
              }}
            >
              <span
                style={{
                  textTransform: "uppercase",
                  fontWeight: 600,
                  color: q.kind === "select" ? "#22c55e" : "#f59e0b",
                  minWidth: 50,
                }}
              >
                {q.kind}
              </span>
              <span style={{ flex: 1 }}>{q.rows} rows</span>
              <span>{q.duration_ms}ms</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
