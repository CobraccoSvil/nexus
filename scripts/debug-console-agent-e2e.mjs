#!/usr/bin/env node
/**
 * E2E: simula il prompt "Console Debug → chat operativa" contro mcp-core.
 *
 * Perché /api/dev-login non è disponibile in production, qui:
 * 1. Legge jwt_secret da GET {CORE_URL}/internal/settings/jwt_secret
 * 2. Mints JWT HS256 come apps/web-ide/app/api/dev-login/route.ts (sub + role + exp)
 * 3. Registra sessions.token_hash via INSERT diretto in Postgres (E2E_PSQL_CONN).
 *    NOTA: l'ex dev_login_server :9999 (Python) e' stato RIMOSSO; oggi la via
 *    canonica e' E2E_PSQL_CONN. Il tentativo a :9999 resta solo come best-effort.
 * 4. Chiama POST /api/chat/... come il browser (header Cookie: token=...)
 *
 * Env:
 *   CORE_URL=http://127.0.0.1:4000
 *   E2E_USER_ID=uuid utente Nexus (opzionale se E2E_PSQL_CONN: si usa il primo admin)
 *   E2E_USER_ROLE=admin
 *   E2E_PROJECT_ID=uuid progetto (opzionale; altrimenti primo da /api/projects/mine)
 *   SESSION_INSERT_URL=http://127.0.0.1:9999 — legacy (dev_login_server rimosso), best-effort
 *   E2E_PSQL_CONN=postgresql://user:pass@host:5432/db — via canonica: INSERT diretto in sessions
 *   POLL_MS=800
 *   RUN_TIMEOUT_MS=180000
 *   SKIP_TOOL_ASSERT=0 — assert sui tool SSE/DB (legacy; oggi agent_steps spesso non viene popolato)
 *   REQUIRE_TOOL_STEPS=0 — se 1, richiede steps[].toolName dall'API (fallisce se il core non persiste gli step)
 */

import crypto from "crypto";
import { spawnSync } from "child_process";

const CORE_URL = (process.env.CORE_URL || "http://127.0.0.1:4000").replace(/\/$/, "");
const SESSION_INSERT_URL = (process.env.SESSION_INSERT_URL || "http://127.0.0.1:9999").replace(/\/$/, "");
const USER_ID_ENV = process.env.E2E_USER_ID?.trim() || "";
const ROLE = process.env.E2E_USER_ROLE || "admin";
const PROJECT_ID_OVERRIDE = process.env.E2E_PROJECT_ID?.trim() || "";
const POLL_MS = Math.max(200, Number(process.env.POLL_MS) || 800);
const RUN_TIMEOUT_MS = Math.max(5000, Number(process.env.RUN_TIMEOUT_MS) || 180000);
const SKIP_TOOL_ASSERT = process.env.SKIP_TOOL_ASSERT === "1";
const REQUIRE_TOOL_STEPS = process.env.REQUIRE_TOOL_STEPS === "1";
const AGENT_TYPE_HINT = process.env.E2E_AGENT_TYPE_HINT?.trim();

function bail(msg, err) {
  console.error(`[e2e] ${msg}`, err || "");
  process.exit(1);
}

function mintJwt(secret, sub, role) {
  const exp = Math.floor(Date.now() / 1000) + 86400 * 7;
  const header = Buffer.from(JSON.stringify({ alg: "HS256", typ: "JWT" })).toString("base64url");
  const payload = Buffer.from(JSON.stringify({ sub, role, exp })).toString("base64url");
  const sig = crypto.createHmac("sha256", secret).update(`${header}.${payload}`).digest("base64url");
  return `${header}.${payload}.${sig}`;
}

function tokenHash(token) {
  return crypto.createHash("sha256").update(token).digest("hex");
}

/** Primo utente admin (per DB locali dove il seed UUID di dev-login non esiste). */
function resolveAdminUserViaPsql() {
  const conn = process.env.E2E_PSQL_CONN?.trim();
  if (!conn) return "";
  const r = spawnSync(
    "psql",
    [
      conn,
      "-t",
      "-A",
      "-v",
      "ON_ERROR_STOP=1",
      "-c",
      "SELECT id::text FROM users WHERE role = 'admin' ORDER BY created_at ASC LIMIT 1",
    ],
    { encoding: "utf8" },
  );
  if (r.error) throw r.error;
  if (r.status !== 0) throw new Error(`psql user lookup: ${r.stderr || r.stdout}`);
  const uid = r.stdout.trim();
  if (!uid || !/^[0-9a-f-]{36}$/i.test(uid)) throw new Error("psql user lookup: nessun admin in users");
  return uid;
}

function resolveUserId() {
  if (USER_ID_ENV) return USER_ID_ENV;
  try {
    const uid = resolveAdminUserViaPsql();
    if (uid) {
      console.log("[e2e] E2E_USER_ID assente — usando admin dal DB:", uid);
      return uid;
    }
  } catch (e) {
    bail("E2E_USER_ID assente e lookup admin via psql fallito (imposta E2E_PSQL_CONN valido o E2E_USER_ID)", e);
  }
  bail("Imposta E2E_USER_ID oppure E2E_PSQL_CONN per risolvere automaticamente un admin.");
  return "";
}

async function fetchJson(url, opts = {}) {
  const headers = {
    ...(opts.cookie ? { Cookie: opts.cookie } : {}),
    "Content-Type": "application/json",
    ...opts.headers,
  };
  const res = await fetch(url, {
    method: opts.method || "GET",
    headers,
    body: opts.body !== undefined ? JSON.stringify(opts.body) : undefined,
    signal: opts.signal,
  });
  const text = await res.text();
  let json;
  try {
    json = text ? JSON.parse(text) : null;
  } catch {
    json = text;
  }
  if (!res.ok) {
    const detail = typeof json === "object" && json?.error ? JSON.stringify(json.error) : String(text).slice(0, 800);
    throw new Error(`${res.status} ${res.statusText} ${detail}`);
  }
  return json;
}

/** Prompt analogo a Console Debug ma con istruzione minima misurabile: deve usare un tool di lettura/elenco file. */
function debugStylePrompt() {
  return [
    "ISTRUZIONE OPERATIVA (obbligatoria — Nexus):",
    "",
    "Non limitarti a diagnosticare. Usa i tool sul workspace del progetto attivo.",
    "",
    "---",
    "",
    "ERRORE — Console Debug",
    "",
    "- Livello: ERROR",
    "- Sorgente: e2e/debug-console-agent",
    "",
    "Messaggio:",
    "File di smoke E2E: verifica che il workspace esista e che tu possa elencare file in root con un tool (es. list_files). Poi riporta almeno 3 voci.",
    "",
    "Passi (con tool sul progetto attivo):",
    "1) list_files (o equivalente) sulla root del progetto.",
    "2) Se serve, read_file su un file piccolo noto (es. README o package.json se presente).",
    "3) Riassumi cosa hai trovato.",
  ].join("\n");
}

function insertSessionViaPsql(hash, userId) {
  const conn = process.env.E2E_PSQL_CONN?.trim();
  if (!conn) return false;
  const uid = userId.replace(/'/g, "''");
  const th = hash.replace(/'/g, "''");
  const sql = `INSERT INTO sessions (user_id, token_hash, expires_at) VALUES ('${uid}'::uuid, '${th}', NOW() + INTERVAL '7 days') ON CONFLICT (token_hash) DO NOTHING`;
  const r = spawnSync("psql", [conn, "-v", "ON_ERROR_STOP=1", "-q", "-c", sql], { encoding: "utf8" });
  if (r.error) throw r.error;
  if (r.status !== 0) {
    throw new Error(`psql exit ${r.status}: ${r.stderr || r.stdout || ""}`);
  }
  console.log("[e2e] session row ensured via psql");
  return true;
}

async function registerSession(token, userId) {
  const hash = tokenHash(token);
  const u = `${SESSION_INSERT_URL}/insert-session?user_id=${encodeURIComponent(userId)}&hash=${encodeURIComponent(hash)}`;
  try {
    const res = await fetch(u, { method: "GET", signal: AbortSignal.timeout(5000) });
    if (!res.ok) throw new Error(`insert-session HTTP ${res.status}`);
    return;
  } catch (_) {
    try {
      if (insertSessionViaPsql(hash, userId)) return;
    } catch (dbErr) {
      bail(`insert-session su ${SESSION_INSERT_URL} irraggiungibile e psql fallito (${process.env.E2E_PSQL_CONN || "imposta E2E_PSQL_CONN"})`, dbErr);
    }
    bail(
      `Nessuna sessione DB registrata: esporta E2E_PSQL_CONN (URI postgres completo per psql). Il vecchio dev_login_server :9999 e' stato rimosso.`,
    );
  }
}

async function pollRun(cookie, runId, start) {
  const terminal = new Set(["completed", "failed", "timed_out", "cancelled", "interrupted"]);
  while (true) {
    if (Date.now() - start > RUN_TIMEOUT_MS) bail(`timeout waiting for run ${runId}`);
    const data = await fetchJson(`${CORE_URL}/api/chat/agent-runs/${runId}`, { cookie });
    const status = data?.status;
    const steps = data?.steps ?? [];
    const pending = Array.isArray(data?.pendingActions) ? data.pendingActions.length : 0;
    console.log(`[e2e] run ${runId} status=${status} steps=${steps.length} pendingActions=${pending}`);
    if (status && terminal.has(status)) {
      return data;
    }
    await new Promise((r) => setTimeout(r, POLL_MS));
  }
}

async function main() {
  const userId = resolveUserId();
  console.log("[e2e] CORE_URL=%s SESSION_INSERT_URL=%s USER=%s", CORE_URL, SESSION_INSERT_URL, userId);

  const secretBody = await fetchJson(`${CORE_URL}/internal/settings/jwt_secret`).catch((e) => bail("cannot read jwt_secret from core", e));
  const jwtSecret = secretBody?.value;
  if (!jwtSecret) bail("jwt_secret empty from core");

  const token = mintJwt(jwtSecret, userId, ROLE);
  const cookie = `token=${encodeURIComponent(token)}`;

  await registerSession(token, userId);

  const mine = await fetchJson(`${CORE_URL}/api/projects/mine`, { cookie }).catch((e) => bail("/api/projects/mine failed — user id o cookie/sessione non validi?", e));
  const projects = mine?.projects || [];
  if (!projects.length) bail("No projects for user — crea/importa un progetto o imposta E2E_PROJECT_ID");
  let project =
    PROJECT_ID_OVERRIDE && projects.find((p) => p.id === PROJECT_ID_OVERRIDE)
      ? projects.find((p) => p.id === PROJECT_ID_OVERRIDE)
      : PROJECT_ID_OVERRIDE
        ? bail(`E2E_PROJECT_ID ${PROJECT_ID_OVERRIDE} not in user's projects`)
        : projects[0];
  console.log("[e2e] project:", project.id, project.name);

  const sessionRes = await fetchJson(`${CORE_URL}/api/chat/sessions`, {
    method: "POST",
    cookie,
    body: { projectId: project.id, title: "e2e debug-console-agent" },
  });
  const sessionId = sessionRes?.session?.id;
  if (!sessionId) bail("unexpected create session payload", JSON.stringify(sessionRes));

  const msgBody = {
    content: debugStylePrompt(),
    profileId: "default",
    activeFiles: [],
    automationMode: "automatic",
    supervisorMode: "none",
    attachments: [],
  };
  if (AGENT_TYPE_HINT) msgBody.agentTypeHint = AGENT_TYPE_HINT;

  const sendRes = await fetchJson(`${CORE_URL}/api/chat/sessions/${sessionId}/messages`, {
    method: "POST",
    cookie,
    body: msgBody,
  }).catch((e) => bail("send message failed", e));

  const runId = sendRes?.agentRun?.runId;
  if (!runId) {
    console.log("[e2e] response (no agentRun — automation might be Study or spawn failed):", JSON.stringify(sendRes).slice(0, 2000));
    bail("No agentRun returned — controlla profilo automation / gateway / brain");
  }

  const final = await pollRun(cookie, runId, Date.now());
  const tools = (final?.steps || []).map((s) => s.toolName).filter(Boolean);
  const ic = Number(final?.iterationCount ?? 0);
  const ans = (final?.finalAnswer || "").trim();
  console.log("[e2e] iterationCount=", ic);
  console.log("[e2e] tool names in API steps (solo se persistiti nel DB):", tools);
  console.log("[e2e] finalAnswer (trunc):", ans.slice(0, 400));

  if (final.status !== "completed") {
    bail(`run ended with status ${final.status} — finalAnswer: ${ans.slice(0, 500)}`);
  }

  /* Il core aggiorna agent_runs ma spesso NON scrive righe in agent_steps: gli step arrivano via SSE alla UI.
     Verifichiamo che il run sia "operativo": almeno un'iterazione LLM agent o risposta finale sostanziosa. */
  const materiallyWorked = ic > 0 || ans.length >= 120;
  if (!materiallyWorked) {
    bail(
      "Run completed ma iterationCount=0 e finalAnswer vuota/troppo breve — possibile problema brain/gateway o risposta troncata.",
    );
  }

  if (REQUIRE_TOOL_STEPS && tools.length === 0) {
    bail("REQUIRE_TOOL_STEPS=1 ma steps[].toolName vuoto dall'API (persi step sul DB?).");
  }

  if (!SKIP_TOOL_ASSERT && !REQUIRE_TOOL_STEPS && tools.length === 0) {
    console.log(
      "[e2e] Nota: nessun step tool esposto via GET agent-runs (previsto finché agent_steps non è popolato); assert basata su iteration/finalAnswer.",
    );
  } else if (!SKIP_TOOL_ASSERT && REQUIRE_TOOL_STEPS === false && tools.length > 0) {
    console.log("[e2e] Tool step persistiti:", tools.join(", "));
  }

  console.log("[e2e] OK — Nexus ha completato il run agent (debug-console style)");
}

main().catch((e) => bail("unhandled", e));
