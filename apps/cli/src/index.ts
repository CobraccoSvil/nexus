const API_BASE = process.env.API_URL || "http://localhost:4000";
// Neural ora e' servito da mcp-core sotto il prefisso /api/neural (il brain Python e' stato eliminato).
// Override esplicito via NEURAL_URL, altrimenti deriva dall'URL del core.
const NEURAL_BASE = process.env.NEURAL_URL || `${API_BASE}/api/neural`;

const HELP = `
AI-Orchestrator CLI v0.2.0

Usage: ai-orchestrator <command> [options]

Commands:
  health        Check platform health (MCP Core + Neural)
  dashboard     Show dashboard metrics
  chat <msg>    Send a message to AI assistant
  analyze <f>   Count TODO/FIXME/HACK/XXX markers and .unwrap() calls in a file
  providers     List available LLM providers and models
  intent <msg>  Classify intent of a message
  route <msg>   Route a message to the best model
  help          Show this help message

Environment:
  API_URL       MCP Core URL (default: http://localhost:4000)
  NEURAL_URL    Neural (mcp-core) URL override (default: <API_URL>/api/neural)
`;

async function fetchJson(url: string, init?: RequestInit): Promise<unknown> {
  const res = await fetch(url, {
    ...init,
    headers: { "Content-Type": "application/json", ...init?.headers },
  });
  if (!res.ok) {
    throw new Error(`HTTP ${res.status}: ${res.statusText}`);
  }
  return res.json();
}

async function cmdHealth() {
  console.log("Checking platform health...\n");
  try {
    const core = (await fetchJson(`${API_BASE}/api/health`)) as Record<string, unknown>;
    console.log("MCP Core:    OK");
    const components = core.components as Record<string, boolean>;
    console.log(`  Database:  ${components.database ? "OK" : "OFFLINE"}`);
    console.log(`  Redis:     ${components.redis ? "OK" : "OFFLINE"}`);
  } catch (e) {
    console.log(`MCP Core:    OFFLINE (${(e as Error).message})`);
  }

  try {
    const neural = (await fetchJson(`${NEURAL_BASE}/health`)) as Record<string, string>;
    console.log(`Neural:      ${neural.status === "ok" ? "OK" : "ERROR"}`);
  } catch (e) {
    console.log(`Neural:      OFFLINE (${(e as Error).message})`);
  }
}

async function cmdDashboard() {
  const data = (await fetchJson(`${API_BASE}/api/dashboard`)) as Record<string, unknown>;
  console.log("Dashboard Metrics");
  console.log("─".repeat(40));
  console.log(`Total runs:       ${data.total_runs}`);
  console.log(`Tokens consumed:  ${(data.tokens_consumed as number).toLocaleString()}`);
  console.log(`Tokens saved:     ${(data.tokens_saved as number).toLocaleString()}`);
  console.log(`Quality findings: ${data.quality_findings}`);
  console.log(`Active jobs:      ${data.active_jobs}`);
}

async function cmdChat(message: string) {
  if (!message) {
    console.error("Usage: ai-orchestrator chat <message>");
    process.exit(1);
  }
  console.log(`You: ${message}\n`);
  const data = (await fetchJson(`${API_BASE}/api/chat`, {
    method: "POST",
    body: JSON.stringify({ project_id: "cli", profile_id: "default", message }),
  })) as Record<string, unknown>;
  console.log(`AI [${data.provider}/${data.model}]: ${data.content}`);
  console.log(`\n(${data.tokens_used} tokens used)`);
}

async function cmdAnalyze(filePath: string) {
  if (!filePath) {
    console.error("Usage: ai-orchestrator analyze <file>");
    process.exit(1);
  }
  const fs = await import("node:fs");
  if (!fs.existsSync(filePath)) {
    console.error(`File not found: ${filePath}`);
    process.exit(1);
  }
  const source = fs.readFileSync(filePath, "utf-8");
  // Conteggio testuale locale, e il comando lo dichiara. Prima l'output si
  // intitolava "quality issues" e un commento prometteva "use the quality
  // endpoint if available": l'endpoint non veniva mai chiamato (e non sarebbe
  // nemmeno applicabile — la scansione qualita' lavora per PROGETTO, non per
  // singolo file), quindi due grep passavano per un'analisi di qualita'.
  console.log(`Scansione marker in ${filePath}\n`);
  console.log(`File: ${filePath}`);
  console.log(`Lines: ${source.split("\n").length}`);
  console.log(`Size: ${source.length} bytes`);

  const todos = (source.match(/TODO|FIXME|HACK|XXX/gi) || []).length;
  const unwraps = (source.match(/\.unwrap\(\)/g) || []).length;
  console.log(`\nMarker trovati (conteggio testuale, non un'analisi semantica):`);
  if (todos > 0) console.log(`  - ${todos} marker TODO/FIXME/HACK/XXX`);
  if (unwraps > 0) console.log(`  - ${unwraps} chiamate .unwrap()`);
  if (todos === 0 && unwraps === 0) console.log(`  (nessuno)`);
}

async function cmdProviders() {
  console.log("LLM Providers\n");
  // Provider e modelli dal catalogo (`/api/models` -> ai_price_catalog): la
  // lista era fissata a openai/anthropic/google, quindi il comando taceva su
  // deepseek, mistral, groq, openrouter e vertex — tutti provider configurati
  // e instradabili. Un elenco scritto a mano invecchia a ogni provider nuovo.
  const data = (await fetchJson(`${API_BASE}/api/models`)) as {
    models?: { provider: string; model: string }[];
  };
  const models = data.models ?? [];
  if (models.length === 0) {
    console.log("(catalogo vuoto o non disponibile)");
    return;
  }
  const byProvider = new Map<string, string[]>();
  for (const m of models) {
    const list = byProvider.get(m.provider) ?? [];
    list.push(m.model);
    byProvider.set(m.provider, list);
  }
  for (const provider of [...byProvider.keys()].sort()) {
    console.log(`${provider}: ${(byProvider.get(provider) ?? []).sort().join(", ")}`);
  }
}

async function cmdIntent(message: string) {
  const data = (await fetchJson(`${NEURAL_BASE}/classify-intent`, {
    method: "POST",
    body: JSON.stringify({ project_id: "cli", profile_id: "default", message }),
  })) as Record<string, string>;
  console.log(`Message:    "${message}"`);
  console.log(`Intent:     ${data.intent}`);
  console.log(`Confidence: ${data.confidence}`);
}

async function cmdRoute(message: string) {
  const data = (await fetchJson(`${NEURAL_BASE}/route-model`, {
    method: "POST",
    body: JSON.stringify({ project_id: "cli", profile_id: "default", message }),
  })) as Record<string, string>;
  console.log(`Message:    "${message}"`);
  console.log(`Intent:     ${data.intent}`);
  console.log(`Provider:   ${data.provider}`);
  console.log(`Model:      ${data.model}`);
  console.log(`Rationale:  ${data.rationale}`);
}

async function main() {
  const args = process.argv.slice(2);
  const command = args[0] || "help";
  const rest = args.slice(1).join(" ");

  try {
    switch (command) {
      case "health":
        await cmdHealth();
        break;
      case "dashboard":
        await cmdDashboard();
        break;
      case "chat":
        await cmdChat(rest);
        break;
      case "analyze":
        await cmdAnalyze(args[1]);
        break;
      case "providers":
        await cmdProviders();
        break;
      case "intent":
        await cmdIntent(rest);
        break;
      case "route":
        await cmdRoute(rest);
        break;
      case "help":
      case "--help":
      case "-h":
        console.log(HELP);
        break;
      default:
        console.error(`Unknown command: ${command}`);
        console.log(HELP);
        process.exit(1);
    }
  } catch (e) {
    console.error(`Error: ${(e as Error).message}`);
    process.exit(1);
  }
}

main();
