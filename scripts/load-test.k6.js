/**
 * Nexus LLM Gateway — Load Test (k6)
 *
 * Target: 100 req/s sostenute, latenza p95 < 3s end-to-end.
 * Uso: k6 run scripts/load-test.k6.js -e GATEWAY_URL=http://localhost:3001
 *
 * Prerequisiti: k6 installato (https://k6.io/docs/get-started/installation/)
 */
import http from "k6/http";
import { check, sleep } from "k6";
import { Counter, Rate, Trend } from "k6/metrics";

// ── Configurazione ────────────────────────────────────────────────────────────

const GATEWAY_URL = __ENV.GATEWAY_URL || "http://localhost:3001";
const JWT_TOKEN   = __ENV.NEXUS_TEST_TOKEN || "test-token-change-me";

// Metriche custom
const llmLatency  = new Trend("llm_latency_ms", true);
const tier3Blocks = new Counter("tier3_blocks_total");
const dlpBlocks   = new Counter("dlp_blocks_total");
const errorRate   = new Rate("error_rate");

// ── Scenari ───────────────────────────────────────────────────────────────────

export const options = {
  scenarios: {
    // Rampa: 0 → 100 VU in 60s, steady 120s, rampa down 30s
    sustained_load: {
      executor: "ramping-vus",
      startVUs: 0,
      stages: [
        { duration: "60s", target: 100 },  // warm-up
        { duration: "120s", target: 100 }, // steady state
        { duration: "30s",  target: 0 },   // wind-down
      ],
    },
    // Spike: 200 VU per 30s per testare resilienza
    spike: {
      executor: "constant-vus",
      vus: 200,
      duration: "30s",
      startTime: "210s",  // parte dopo il test sostenuto
    },
  },
  thresholds: {
    // p95 < 3s su tutte le richieste
    "http_req_duration{scenario:sustained_load}": ["p(95)<3000"],
    // p99 < 5s
    "http_req_duration{scenario:sustained_load}": ["p(99)<5000"],
    // < 1% error rate nel sustained load
    "error_rate{scenario:sustained_load}": ["rate<0.01"],
    // latency LLM (include provider call) p95 < 5s
    "llm_latency_ms": ["p(95)<5000"],
    // HTTP request failures < 2%
    "http_req_failed": ["rate<0.02"],
  },
};

// ── Dataset di prompt di test ─────────────────────────────────────────────────

const PROMPTS = [
  "Write a Python function that calculates the Fibonacci sequence recursively",
  "Explain the difference between REST and GraphQL APIs",
  "What is the time complexity of merge sort?",
  "Write a SQL query to find all users who have not logged in for 30 days",
  "Generate a TypeScript interface for a User with name, email and role",
  "Explain async/await in JavaScript with a simple example",
  "What are SOLID principles in software engineering?",
  "Write a regex to validate an Italian fiscal code (codice fiscale)",
  "How does pgvector work for similarity search?",
  "What is the difference between Docker and Kubernetes?",
];

const TENANTS = ["tenant-a", "tenant-b", "tenant-c", "tenant-d"];
const FEATURES = ["code-review", "doc-generation", "chat", "analysis"];

function randomItem(arr) {
  return arr[Math.floor(Math.random() * arr.length)];
}

// ── Scenario: chiamata LLM tier 0 (bulk) ─────────────────────────────────────

export default function () {
  const tenantId  = randomItem(TENANTS);
  const prompt    = randomItem(PROMPTS);
  const feature   = randomItem(FEATURES);
  const requestId = `load-${Date.now()}-${Math.random().toString(36).slice(2)}`;

  const payload = JSON.stringify({
    model: "coder-small",
    messages: [{ role: "user", content: prompt }],
    max_tokens: 256,
    metadata: {
      tenant_id: tenantId,
      user_id:   `user-${tenantId}`,
      request_id: requestId,
      sensitivity_tier: 0,
      feature,
    },
  });

  const start = Date.now();
  const res = http.post(`${GATEWAY_URL}/v1/complete`, payload, {
    headers: {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${JWT_TOKEN}`,
    },
    timeout: "10s",
  });
  llmLatency.add(Date.now() - start);

  const ok = check(res, {
    "status 200":           (r) => r.status === 200,
    "has content field":    (r) => {
      try { return !!JSON.parse(r.body).content; } catch { return false; }
    },
    "provider is set":      (r) => {
      try { return !!JSON.parse(r.body).provider_used; } catch { return false; }
    },
    "finish_reason = stop": (r) => {
      try { return JSON.parse(r.body).finish_reason === "stop"; } catch { return false; }
    },
  });

  if (!ok) errorRate.add(1);
  else       errorRate.add(0);

  if (res.status === 403) {
    const body = JSON.parse(res.body || "{}");
    if (body.code === "TIER_BLOCKED") tier3Blocks.add(1);
    if (body.code === "DLP_BLOCKED")  dlpBlocks.add(1);
  }

  sleep(0.1); // 10ms think time → ~100 req/s con 10 VU; adattare ai VU target
}

// ── Scenario separato: tier 3 (deve essere bloccato in cloud) ─────────────────

export function tier3Attack() {
  const res = http.post(`${GATEWAY_URL}/v1/complete`, JSON.stringify({
    model: "sensitive-only",
    messages: [{ role: "user", content: "Process this sensitive document" }],
    metadata: {
      tenant_id:        "attacker-tenant",
      user_id:          "attacker",
      request_id:       `t3-${Date.now()}`,
      sensitivity_tier: 3,
      feature:          "test",
    },
  }), {
    headers: {
      "Content-Type": "application/json",
      "Authorization": `Bearer ${JWT_TOKEN}`,
    },
  });

  check(res, {
    "tier 3 blocked (403)": (r) => r.status === 403,
  });

  tier3Blocks.add(res.status === 403 ? 1 : 0);
}

// ── Summary report ────────────────────────────────────────────────────────────

export function handleSummary(data) {
  const p95 = data.metrics.http_req_duration?.values?.["p(95)"] ?? 0;
  const p99 = data.metrics.http_req_duration?.values?.["p(99)"] ?? 0;
  const rps  = data.metrics.http_reqs?.values?.rate ?? 0;
  const errs = data.metrics.error_rate?.values?.rate ?? 0;

  const report = [
    "═══════════════════════════════════════════════",
    "  Nexus Load Test — Summary",
    "═══════════════════════════════════════════════",
    `  RPS (peak):     ${rps.toFixed(1)} req/s`,
    `  Latency p95:    ${p95.toFixed(0)} ms  (target: < 3000)`,
    `  Latency p99:    ${p99.toFixed(0)} ms  (target: < 5000)`,
    `  Error rate:     ${(errs * 100).toFixed(2)}%  (target: < 1%)`,
    `  Tier-3 blocks:  ${data.metrics.tier3_blocks_total?.values?.count ?? 0}`,
    `  DLP blocks:     ${data.metrics.dlp_blocks_total?.values?.count ?? 0}`,
    "═══════════════════════════════════════════════",
  ].join("\n");

  console.log(report);

  return {
    stdout: report,
    "reports/load-test-result.json": JSON.stringify(data, null, 2),
  };
}
