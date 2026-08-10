import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import {
  renderReadiness,
  type GatewayProvider,
} from "./gateway-providers.ts";

const qui = dirname(fileURLToPath(import.meta.url));

/**
 * La fixture e' il wire VERO, catturato il 10/08/2026 da
 * `GET /api/internal/providers/status` su mcp-core in esercizio — la stessa
 * composizione che `/api/gateway/providers` rilancia al frontend
 * (`nexus-gateway::server::routes::fetch_providers_from_mcp_core`).
 *
 * Non e' un oggetto scritto a mano: un input fabbricato fisserebbe l'assunto da
 * verificare, ed e' esattamente il modo in cui il difetto e' sopravvissuto —
 * nessuno aveva mai confrontato cio' che il backend MANDA con cio' che la
 * pagina LEGGE (regola O).
 *
 * Cio' che la fixture NON contiene, dichiarato invece di essere taciuto: i tre
 * campi che il ramo online aggiunge (`last_check`, `latency_ms`, `error_kind`).
 * Nessuno dei tre e' letto da `renderReadiness`, quindi la loro assenza non
 * indebolisce questo test; se un giorno lo diventassero, la fixture va rifatta
 * dall'endpoint che la UI interroga davvero.
 */
function wire(): GatewayProvider[] {
  const raw = readFileSync(join(qui, "__wire__", "gateway-providers.json"), "utf8");
  return (JSON.parse(raw) as { providers: GatewayProvider[] }).providers;
}

test("il wire reale porta la prontezza su ogni provider", () => {
  const providers = wire();
  assert.ok(providers.length >= 5, "la fixture deve contenere il parco provider reale");
  const senza = providers.filter((p) => !p.readiness);
  assert.deepEqual(
    senza.map((p) => p.name),
    [],
    "ogni entry del wire dichiara la propria prontezza: se questo cade, il difetto e' nel produttore",
  );
});

test("non_configured non si rende come 'mai misurato'", () => {
  // Il caso misurato: vllm arriva `not_configured` e la pagina scriveva «mai
  // misurato», che e' il significato di awaiting_first_probe — cioe' diceva il
  // falso proprio dove un amministratore va a capire cosa fare.
  const p: GatewayProvider = { name: "vllm", healthy: null, readiness: "not_configured" };
  const reso = renderReadiness(p);
  assert.equal(reso.label, "non configurato");
  assert.equal(reso.requiresAction, false);
  assert.notEqual(reso.label, "mai misurato");
});

test("le tre varianti senza misura restano distinte", () => {
  const etichette = (["not_configured", "awaiting_first_probe", "stalled"] as const).map(
    (readiness) => renderReadiness({ name: "x", healthy: null, readiness }).label,
  );
  assert.equal(
    new Set(etichette).size,
    3,
    `tre varianti con rimedi opposti devono dare tre frasi diverse: ${etichette.join(" | ")}`,
  );
});

test("lo stallo e' l'unica variante che chiede un intervento", () => {
  // E' il campo su cui un allarme puo' decidere. Senza, uno stallo e' una
  // stringa come le altre e nessuno lo distingue da un'attesa.
  const varianti = ["not_configured", "awaiting_first_probe", "stalled", "healthy", "down"] as const;
  const richiedono = varianti.filter(
    (readiness) => renderReadiness({ name: "x", healthy: null, readiness }).requiresAction,
  );
  assert.deepEqual(richiedono, ["stalled"]);
});

test("il cooldown ha la precedenza che aveva gia'", () => {
  // `cooldown_seconds_remaining` e' scritto da mcp-core indipendentemente dalla
  // prontezza: perdere la sua precedenza sarebbe una regressione silenziosa
  // rispetto a cio' che ide-shell mostra oggi.
  const reso = renderReadiness({
    name: "anthropic",
    healthy: false,
    readiness: "down",
    error: "credit_balance_too_low",
    cooldown_seconds_remaining: 21377,
  });
  assert.match(reso.label, /in pausa per \d+ min/);
});

test("una entry senza prontezza dichiara l'assenza, non una misura", () => {
  // Backend che non parla questa versione del contratto: l'ignoto e' una
  // variante dichiarata, non un valore comodo (regola Q).
  const reso = renderReadiness({ name: "vecchio", healthy: null });
  assert.equal(reso.label, "prontezza non dichiarata");
  assert.equal(reso.requiresAction, false);
});

test("il difetto reale: derivare l'etichetta dal solo healthy la sbaglia", () => {
  // MUTAZIONE dichiarata: se `renderReadiness` tornasse a guardare il solo
  // `healthy`, tutte le entry con `healthy === null` darebbero la stessa
  // frase. Qui si verifica che sul wire REALE quel collasso non avvenga.
  const senzaMisura = wire().filter((p) => p.healthy === null || p.healthy === undefined);
  const etichette = new Set(senzaMisura.map((p) => renderReadiness(p).label));
  if (senzaMisura.length > 0) {
    assert.ok(
      etichette.size >= 1 && !etichette.has("mai misurato"),
      `nessuna entry senza misura deve uscire come 'mai misurato': ${[...etichette].join(" | ")}`,
    );
  }
});
