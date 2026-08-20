import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import {
  renderDeclaration,
  renderReadiness,
  renderSelectability,
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
  // ...ma senza mangiarsi la causa: la pausa dice QUANDO torna, il credito
  // dice COSA fare perche' torni davvero.
  assert.match(reso.label, /credit_balance_too_low/);
});

test("una pausa senza causa nota non inventa una causa", () => {
  const reso = renderReadiness({
    name: "x",
    healthy: null,
    readiness: "down",
    cooldown_seconds_remaining: 120,
  });
  assert.equal(reso.label, "in pausa per 2 min");
});

test("una entry senza prontezza dichiara l'assenza, non una misura", () => {
  // Backend che non parla questa versione del contratto: l'ignoto e' una
  // variante dichiarata, non un valore comodo (regola Q).
  const reso = renderReadiness({ name: "vecchio", healthy: null });
  assert.equal(reso.label, "prontezza non dichiarata");
  assert.equal(reso.requiresAction, false);
});

test("il difetto misurato: un fornitore sano puo' essere interamente non dichiarato", () => {
  // IL CASO CHE GIUSTIFICA IL CAMPO. Sul wire REALE groq e openrouter arrivano
  // `healthy` — e sono anche i due che non hanno una sola riga di capability.
  // Finche' la pagina leggeva la sola prontezza, di loro diceva «attivo» e
  // nient'altro: l'assenza non aveva dove comparire.
  const perNome = new Map(wire().map((p) => [p.name, p]));
  for (const nome of ["groq", "openrouter"]) {
    const p = perNome.get(nome);
    assert.ok(p, `${nome} deve essere nella fixture`);
    assert.equal(renderReadiness(p).label, "attivo", `${nome} e' sano, e resta sano`);
    assert.equal(renderReadiness(p).requiresAction, false);
    const d = renderDeclaration(p);
    assert.ok(d, `${nome} non ha capability: l'assenza deve avere una resa`);
    assert.equal(d.requiresAction, true, "nessun ciclo la completa: serve un intervento");
    assert.match(d.label, /capability/);
  }
});

test("assente e parziale non si rendono con la stessa frase", () => {
  // Due rimedi diversi: una migrazione di onboarding mancante contro un
  // catalogo che ha aggiunto modelli dopo. Una frase sola manderebbe a cercare
  // la cosa sbagliata.
  const assente = renderDeclaration({
    name: "openrouter",
    healthy: true,
    declaration: "absent",
    declaration_undeclared: 17,
  });
  const parziale = renderDeclaration({
    name: "openai",
    healthy: true,
    declaration: "partial",
    declaration_undeclared: 11,
  });
  assert.notEqual(assente?.label, parziale?.label);
  assert.match(assente?.label ?? "", /17/);
  assert.match(parziale?.label ?? "", /11/);
});

test("cio' che e' dichiarato per intero non produce rumore", () => {
  // `complete` e `nothing_to_declare` non hanno nulla da dire: una riga per
  // ogni fornitore in ordine e' il modo in cui quelle che contano smettono di
  // essere lette.
  for (const declaration of ["complete", "nothing_to_declare"] as const) {
    assert.equal(renderDeclaration({ name: "x", healthy: true, declaration }), null);
  }
});

test("una entry senza dichiarazione non inventa una mancanza", () => {
  // Backend che non parla questa versione del contratto: di lui non sappiamo se
  // manchi qualcosa, e l'ignoto non diventa un allarme (regola Q).
  assert.equal(renderDeclaration({ name: "vecchio", healthy: true }), null);
});

test("le due domande restano due campi", () => {
  // MUTAZIONE dichiarata: se la copertura fosse stata infilata dentro
  // `readiness` come variante di stallo, questa entry non sarebbe
  // rappresentabile — `healthy` e `stalled` sono lo stesso campo. Il test la
  // costruisce apposta, ed e' lo stato reale di groq.
  const p: GatewayProvider = {
    name: "groq",
    healthy: true,
    readiness: "healthy",
    declaration: "absent",
    declaration_undeclared: 2,
  };
  assert.equal(renderReadiness(p).requiresAction, false, "la salute e' misurata e va bene");
  assert.equal(renderDeclaration(p)?.requiresAction, true, "la dichiarazione no");
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

/**
 * LA TERZA DOMANDA — selezionabilita' (20/08/2026).
 *
 * La fixture e' del 10/08/2026 e NON contiene `selectability`: e' wire reale
 * catturato prima che il campo esistesse, e aggiungerlo a mano sarebbe
 * fabbricare un input, cioe' fissare l'assunto da verificare (regola O). Cio'
 * che sulla fixture VERA si puo' provare — e conta — e' che un backend che non
 * parla questa versione del contratto non produca allarmi.
 */
test("un wire senza selectability non inventa un allarme", () => {
  const providers = wire();
  const senzaCampo = providers.filter((p) => !p.selectability);
  assert.equal(
    senzaCampo.length,
    providers.length,
    "premessa: la fixture precede il campo, e questo test vale su di essa",
  );
  const resi = senzaCampo.map((p) => renderSelectability(p));
  assert.deepEqual(
    resi.filter((r) => r !== null),
    [],
    "l'ignoto non degrada in un allarme: senza il campo non sappiamo se il gate lo ammetta",
  );
});

test("i due silenzi della selezionabilita' restano due frasi", () => {
  // E' l'intera ragione per cui il campo esiste. Lo stato reale del
  // 20/08/2026: groq e' fermo su giri che non lo misurano da 36 giorni,
  // perplexity aspetta soltanto la prima qualificazione. Sul pannello erano
  // indistinguibili — entrambi semplicemente assenti dal routing.
  const groq = renderSelectability({
    name: "groq",
    healthy: true,
    selectability: "stuck_unmeasured",
    selectability_stuck_models: 2,
  });
  const perplexity = renderSelectability({
    name: "perplexity",
    healthy: true,
    selectability: "awaiting_measurement",
  });
  assert.ok(groq && perplexity);
  assert.notEqual(
    groq.label,
    perplexity.label,
    "due stati con rimedi opposti non possono dare la stessa frase",
  );
  assert.equal(groq.requiresAction, true, "un blocco non si scioglie aspettando");
  assert.equal(perplexity.requiresAction, false, "un'attesa si scioglie da sola");
  assert.match(groq.label, /2 modelli/, "quanti sono fermi dimensiona l'intervento");
});

test("le tre domande restano tre campi", () => {
  // MUTAZIONE dichiarata: se la selezionabilita' fosse stata infilata in
  // `readiness` o in `declaration`, questa entry non sarebbe rappresentabile.
  // E' lo stato reale di groq il 20/08/2026: sano, scoperto e fuori dal
  // routing insieme, con tre rimedi diversi.
  const p: GatewayProvider = {
    name: "groq",
    healthy: true,
    readiness: "healthy",
    declaration: "absent",
    declaration_undeclared: 3,
    selectability: "stuck_unmeasured",
    selectable_for_routing: false,
    selectability_stuck_models: 2,
  };
  assert.equal(renderReadiness(p).requiresAction, false, "la salute e' misurata e va bene");
  assert.equal(renderDeclaration(p)?.requiresAction, true, "la dichiarazione manca");
  assert.equal(renderSelectability(p)?.requiresAction, true, "e il gate non lo ammette");
});

test("un'esclusione misurata non e' un allarme", () => {
  // `measured_not_qualified` e' il fornitore che la batteria HA guardato e non
  // ha promosso: nessun intervento lo cambierebbe, quindi non si mostra.
  for (const selectability of ["selectable", "gate_off", "nothing_to_select", "measured_not_qualified"] as const) {
    assert.equal(
      renderSelectability({ name: "x", healthy: true, selectability }),
      null,
      `${selectability} non deve produrre una riga`,
    );
  }
});
