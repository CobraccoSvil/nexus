/**
 * GET /api/system/services
 *
 * Ritorna lo stato dei servizi di sistema di Nexus (nexus-core, nexus-gateway, ecc.)
 * tramite `systemctl is-active`. Non dipende da mcp-core: funziona anche quando
 * l'orchestratore è down, permettendo all'utente di riavviarlo dall'IDE.
 *
 * Campo extra `port_alive`: true se la porta TCP risponde indipendentemente
 * dallo stato systemd. Utile per rilevare processi avviati fuori da systemd
 * (es. via deploy script diretto).
 */
import { NextResponse } from "next/server";
import { exec } from "child_process";
import { promisify } from "util";
import * as net from "net";

export const runtime = "nodejs";

const execAsync = promisify(exec);

export interface NexusServiceInfo {
  name: string;
  label: string;
  port: number;
  description: string;
  /** LED della statusbar controllato da questo servizio (null = nessun LED diretto). */
  led?: string;
  /**
   * true = servizio system (postgres, redis). Mostrabile ma non controllabile
   * dall'IDE senza privilegi root. I pulsanti start/stop/restart vengono omessi.
   */
  readonly?: boolean;
  /** Nome del container Docker (per servizi containerizzati: Postgres/Redis). */
  dockerContainer?: string;
  state: "active" | "inactive" | "failed" | "activating" | "unknown";
  sub_state?: string;
  /**
   * true se la porta TCP risponde, indipendentemente dallo stato systemd.
   * Permette di distinguere "inattivo in systemd ma processo vivo" da "veramente spento".
   */
  port_alive?: boolean;
}

// Servizi Nexus mostrati all'utente. nexus-webide è escluso per evitare
// che l'utente si tagli fuori riavviando il web-ide da cui sta operando.
// led: nome del LED nella statusbar che questo servizio alimenta.
// readonly: servizi system (root), mostrati ma non controllabili dall'IDE.
const NEXUS_SERVICES: Omit<NexusServiceInfo, "state" | "sub_state">[] = [
  // Il brain Python (nexus-neural-wsl, :8001) e' stato eliminato: gli endpoint
  // AI sono ora ri-esposti in mcp-core (porta 4000) sotto /api/neural. Il LED
  // "Brain" e' stato rimosso dalla statusbar (consolidato nel LED del Core).
  { name: "nexus-core-wsl",       label: "Core (mcp-core)",  port: 4000, description: "Orchestratore + endpoint AI (/api/neural) + Tool Runner gRPC :50071", led: "Core" },
  { name: "nexus-gateway",        label: "LLM Gateway",      port: 4060, description: "Router provider AI",                      led: "OpenAI · Anthropic · …" },
  { name: "nexus-plugin-wsl",     label: "Plugin Service",   port: 4050, description: "Connettori MCP" },
  { name: "nexus-admin-wsl",      label: "Admin Service",    port: 4010, description: "Pannello amministrazione" },
  // Servizi di infrastruttura (system): sola lettura, richiedono root per il controllo.
  // In setup container (Nexus dev WSL) il check via systemctl non funziona:
  // Postgres/Redis girano in container ideai-* e il port mapping non e' 1:1
  // (es. Postgres host :5433 -> container :5432). Per questi servizi
  // checkContainer() interroga `docker inspect` come source-of-truth.
  { name: "redis-server",         label: "Redis",            port: 6379, description: "Cache e broker messaggi",                 led: "Redis",    readonly: true, dockerContainer: "ideai-redis-1" },
  { name: "postgresql@16-main",   label: "PostgreSQL",       port: 5433, description: "Database relazionale principale",         led: "DB",       readonly: true, dockerContainer: "ideai-postgres-nexus-1" },
];

/**
 * Fix M29: legge stato di un container Docker (running/exited/...).
 * Usato per Postgres/Redis che girano containerizzati in ideai-*.
 */
async function checkDockerContainer(name: string): Promise<{ state: string; sub_state: string }> {
  try {
    const { stdout } = await execAsync(
      `docker inspect ${name} --format "{{.State.Status}}|{{.State.Health.Status}}" 2>/dev/null || echo "missing|"`
    );
    const trimmed = stdout.trim();
    if (trimmed === "missing|" || trimmed === "") {
      return { state: "unknown", sub_state: "container-not-found" };
    }
    const [status, health] = trimmed.split("|");
    if (status === "running") {
      return { state: "active", sub_state: health || "running" };
    }
    return { state: "inactive", sub_state: status || "dead" };
  } catch {
    return { state: "unknown", sub_state: "unknown" };
  }
}

/**
 * Verifica se una porta TCP locale risponde entro `timeoutMs` millisecondi.
 * Prova prima su 127.0.0.1, poi su ::1 (IPv6 loopback) come fallback.
 * Non invia dati HTTP: basta che la connessione venga accettata.
 */
function checkPort(port: number, timeoutMs = 1200): Promise<boolean> {
  function tryConnect(host: string): Promise<boolean> {
    return new Promise((resolve) => {
      const sock = new net.Socket();
      let settled = false;
      const done = (alive: boolean) => {
        if (settled) return;
        settled = true;
        sock.destroy();
        resolve(alive);
      };
      sock.setTimeout(timeoutMs);
      sock.once("connect", () => done(true));
      sock.once("error",   () => done(false));
      sock.once("timeout", () => done(false));
      sock.connect(port, host);
    });
  }
  // Prima IPv4, poi IPv6 come fallback
  return tryConnect("127.0.0.1").then(ok => ok ? true : tryConnect("::1"));
}

function parseSystemctlOutput(stdout: string): { state: string; sub_state: string } {
  const props: Record<string, string> = {};
  for (const l of stdout.trim().split("\n")) {
    const i = l.indexOf("=");
    if (i > 0) props[l.slice(0, i).trim()] = l.slice(i + 1).trim();
  }
  return {
    state: props["ActiveState"] ?? "unknown",
    sub_state: props["SubState"] ?? "unknown",
  };
}

async function getServiceState(name: string, systemScope = false): Promise<{ state: string; sub_state: string }> {
  const unit = name.includes("@") ? `"${name}.service"` : `"${name}.service"`;
  try {
    if (systemScope) {
      // Servizi system (redis, postgres): interroga direttamente systemctl senza --user.
      const { stdout } = await execAsync(
        `systemctl show ${unit} --property=ActiveState,SubState --no-pager 2>/dev/null || echo "ActiveState=unknown\nSubState=unknown"`
      );
      return parseSystemctlOutput(stdout);
    }
    // Servizi Nexus: prova --user prima (dev), poi system (prod).
    // NON usare il fallback a system se --user restituisce active/inactive
    // (systemctl --user show su un servizio inesistente dà comunque inactive).
    // Invece, controlla esplicitamente che l'UnitFileState non sia "bad/not-found".
    const { stdout: userOut } = await execAsync(
      `systemctl --user show ${unit} --property=ActiveState,SubState,UnitFileState --no-pager 2>/dev/null || echo "ActiveState=unknown\nSubState=unknown\nUnitFileState=unknown"`
    );
    const props: Record<string, string> = {};
    for (const l of userOut.trim().split("\n")) {
      const i = l.indexOf("=");
      if (i > 0) props[l.slice(0, i).trim()] = l.slice(i + 1).trim();
    }
    const unitFileState = props["UnitFileState"] ?? "unknown";
    // Se il servizio non esiste nell'user scope il UnitFileState è "" o "not-found"
    if (unitFileState && unitFileState !== "not-found" && unitFileState !== "unknown") {
      return { state: props["ActiveState"] ?? "unknown", sub_state: props["SubState"] ?? "unknown" };
    }
    // Fallback a system
    const { stdout: sysOut } = await execAsync(
      `systemctl show ${unit} --property=ActiveState,SubState --no-pager 2>/dev/null || echo "ActiveState=unknown\nSubState=unknown"`
    );
    return parseSystemctlOutput(sysOut);
  } catch {
    return { state: "unknown", sub_state: "unknown" };
  }
}

export async function GET() {
  const services: NexusServiceInfo[] = await Promise.all(
    NEXUS_SERVICES.map(async (svc) => {
      // Fix M29: per servizi con dockerContainer definito (Postgres, Redis)
      // interroga lo stato del container Docker invece di systemctl.
      // In setup non-container il dockerContainer e' undefined e si ricade
      // sul check systemctl standard.
      const stateProbe = svc.dockerContainer
        ? checkDockerContainer(svc.dockerContainer)
        : getServiceState(svc.name, svc.readonly === true);
      const [{ state, sub_state }, port_alive] = await Promise.all([
        stateProbe,
        checkPort(svc.port),
      ]);
      // Fix M28: in dev WSL i servizi Nexus sono lanciati da deploy-local.sh come
      // setsid nohup processes, senza una unit systemd corrispondente. systemctl
      // ritorna ActiveState=unknown/inactive ma la porta risponde. Se non c'è
      // una unit ma la porta è viva, consideriamo il servizio attivo.
      // I servizi "readonly" (postgres/redis) restano come sono perché in
      // setup container hanno port mapping diverso (es. Postgres su :5433).
      const stateUnknown = state === "unknown" || state === "inactive" || state === "failed";
      const effectiveState: NexusServiceInfo["state"] =
        !svc.readonly && stateUnknown && port_alive
          ? "active"
          : (state as NexusServiceInfo["state"]);
      const effectiveSubState =
        !svc.readonly && stateUnknown && port_alive
          ? "running"
          : sub_state;
      return {
        ...svc,
        state: effectiveState,
        sub_state: effectiveSubState,
        port_alive,
      };
    })
  );

  return NextResponse.json({ services });
}
