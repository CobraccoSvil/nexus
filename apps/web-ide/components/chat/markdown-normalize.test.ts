import { strict as assert } from "node:assert";
import { test } from "node:test";

import { normalizeContent } from "./markdown-normalize.ts";

// ── Cio' che le euristiche DEVONO continuare a fare (prosa) ──────────────────

test("spezza il muro di prosa: frase.Frase", () => {
  assert.equal(
    normalizeContent("Ho finito.Ora verifico il resto."),
    "Ho finito.\n\nOra verifico il resto.",
  );
});

test("spezza il flow narrativo: titolo:Sottotitolo", () => {
  assert.equal(normalizeContent("Analisi:Procedo con il fix"), "Analisi:\n\nProcedo con il fix");
});

test("non spezza gli URL (lookbehind sugli schemi)", () => {
  const url = "vedi https://esempio.it/Percorso per i dettagli";
  assert.equal(normalizeContent(url), url);
});

// ── Cio' che NON deve piu' toccare (segmenti letterali) ──────────────────────

test("un blocco di codice resta byte per byte identico", () => {
  // Il difetto reale: `process.env.PORT` diventava `process.env.\n\nPORT`, e
  // `foo.Bar()` -> `foo.\n\nBar()`. Codice corrotto sotto gli occhi dell'utente,
  // con il pulsante "Esegui" accanto sui blocchi shell.
  const code = "```js\nconst p = process.env.PORT;\nReact.Component;\n```";
  assert.equal(normalizeContent(code), code);
});

test("il codice inline resta identico", () => {
  const s = "usa `process.env.NODE_ENV` per distinguere";
  assert.equal(normalizeContent(s), s);
});

test("una riga di tabella non viene spezzata", () => {
  // Difetto osservato il 20/07: la cella con `process.env.PORT` spezzava la
  // riga, GFM chiudeva la tabella e il resto degradava in paragrafo.
  const riga = "| Porte | MEDIA | Nessuna validazione process.env.PORT | server.cjs:68 |";
  assert.equal(normalizeContent(riga), riga);
});

test("la tabella intera sopravvive, e la prosa attorno viene comunque spezzata", () => {
  const input = [
    "Ecco i rischi.Tabella qui sotto:",
    "",
    "| Dominio | Severità | Evidenza |",
    "|---|---|---|",
    "| Porte | MEDIA | process.env.PORT non validato |",
    "| Avvio | ALTA | server.cjs:68-72 |",
  ].join("\n");

  const out = normalizeContent(input);
  const righe = out.split("\n");

  // Ogni riga di tabella e' ancora una riga sola, con le sue pipe.
  const righeTabella = righe.filter((r) => r.trimStart().startsWith("|"));
  assert.equal(righeTabella.length, 4, `attese 4 righe di tabella, trovate: ${righeTabella.length}`);
  assert.ok(
    righeTabella.some((r) => r.includes("process.env.PORT non validato")),
    "la cella con process.env.PORT deve restare intera",
  );
  // La prosa fuori dalla tabella e' stata comunque normalizzata.
  assert.ok(out.includes("Ecco i rischi.\n\nTabella"), "la prosa attorno va spezzata come prima");
});

test("piu' segmenti letterali nello stesso testo tornano al posto giusto", () => {
  const input = "Prima `a.B` poi:\n\n```\nc.D\n```\n\nInfine testo.Altro";
  const out = normalizeContent(input);
  assert.ok(out.includes("`a.B`"), "inline code intatto");
  assert.ok(out.includes("```\nc.D\n```"), "fence intatto");
  assert.ok(out.includes("testo.\n\nAltro"), "la prosa finale e' spezzata");
});

test("un fence non chiuso a fine testo resta comunque protetto", () => {
  const input = "```sh\ncd D:\\IDEAI\nnpm run build.Then";
  assert.equal(normalizeContent(input), input);
});
