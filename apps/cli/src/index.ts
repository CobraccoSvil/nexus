const API_BASE = process.env.API_URL || "http://localhost:4000";
const NEURAL_BASE = process.env.NEURAL_URL || "http://localhost:8001";

const HELP = `
AI-Orchestrator CLI v0.2.0

Usage: ai-orchestrator <command> [options]

Commands:
  health        Check platform health (MCP Core + Neural Core)
  dashboard     Show dashboard metrics
  chat <msg>    Send a message to AI assistant
  analyze <f>   Analyze a source file for quality issues
  providers     List available LLM providers and models
  intent <msg>  Classify intent of a message
  route <msg>   Route a message to the best model
  help          Show this help message

Environment:
  API_URL       MCP Core URL (default: http://localhost:4000)
  NEURAL_URL    Neural Core URL (default: http://localhost:8001)
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
    console.log(`  Neural:    ${components.neural_core ? "OK" : "OFFLINE"}`);
  } catch (e) {
    console.log(`MCP Core:    OFFLINE (${(e as Error).message})`);
  }

  try {
    const neural = (await fetchJson(`${NEURAL_BASE}/health`)) as Record<string, string>;
    console.log(`Neural Core: ${neural.status === "ok" ? "OK" : "ERROR"}`);
  } catch (e) {
    console.log(`Neural Core: OFFLINE (${(e as Error).message})`);
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
  console.log(`Analyzing ${filePath} (${source.split("\n").length} lines)...\n`);
  // Use the quality endpoint if available, otherwise show local analysis
  console.log(`File: ${filePath}`);
  console.log(`Lines: ${source.split("\n").length}`);
  console.log(`Size: ${source.length} bytes`);

  const todos = (source.match(/TODO|FIXME|HACK|XXX/gi) || []).length;
  const unwraps = (source.match(/\.unwrap\(\)/g) || []).length;
  console.log(`\nFindings:`);
  if (todos > 0) console.log(`  - ${todos} TODO/FIXME markers`);
  if (unwraps > 0) console.log(`  - ${unwraps} .unwrap() calls`);
  if (todos === 0 && unwraps === 0) console.log(`  (none)`);
}

async function cmdProviders() {
  console.log("LLM Providers\n");
  for (const provider of ["openai", "anthropic", "google"]) {
    try {
      const data = (await fetchJson(`${NEURAL_BASE}/providers/${provider}/models`)) as Record<string, unknown>;
      const models = data.models as string[];
      console.log(`${provider}: ${models.join(", ")}`);
    } catch {
      console.log(`${provider}: (unavailable)`);
    }
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
