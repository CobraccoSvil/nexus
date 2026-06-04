// Helper puri e tipi condivisi del pannello Database progetto.
// Estratti da project-db-panel.tsx (refactor god-file) senza modifiche di comportamento.

export const statusColorMap: Record<string, string> = {
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
export type DbErrorCategory = "unreachable" | "no_database" | "auth_failed" | "tables_missing" | "unknown";

export function categorizeDbError(error: string | null | undefined, tableCount?: number | null): DbErrorCategory {
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
export function parseConnPartsForActions(connStr: string): { host: string; port: string; database: string } | null {
  // postgres://user:pass@host:port/dbname o postgresql:// ; mysql:// ; mssql:// ; ecc.
  const m = connStr.match(/^[a-z]+:\/\/(?:[^@]+@)?([^:/]+)(?::(\d+))?(?:\/([^?]+))?/i);
  if (!m) return null;
  return { host: m[1] ?? "", port: m[2] ?? "", database: (m[3] ?? "").split("?")[0] ?? "" };
}

/** Lista di host candidati quando il primo è unreachable (es. localhost dev → server prod). */
export function alternativeHostsFor(currentHost: string): string[] {
  const local = ["localhost", "127.0.0.1", "::1"];
  if (local.includes(currentHost)) return ["192.168.0.6", "192.168.0.3"];
  if (currentHost === "192.168.0.6") return ["192.168.0.3", "localhost"];
  if (currentHost === "192.168.0.3") return ["192.168.0.6", "localhost"];
  return ["localhost", "192.168.0.6", "192.168.0.3"].filter((h) => h !== currentHost);
}

/** Parser per estrarre host/port/db/user/pass da una connection string. */
export function parseConnectionString(
  raw: string,
): { host: string; port: string; database: string; username: string; password: string } | null {
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
}

export interface InitForm {
  name: string;
  engine: string;
  hosting_mode: string;
  migration_tool: string;
  migration_path: string;
  allow_ddl_override: boolean;
  connection_string: string;
}

export interface ConnFields {
  host: string;
  port: string;
  database: string;
  username: string;
  password: string;
}

export interface DetectedConfig {
  engine?: string;
  hosting_mode?: string;
  migration_tool?: string;
  migration_path?: string;
  connection_string?: string;
  hints: string[];
}
