// Unit test della resa leggibile di parametri e risultato tool (ADR 0037).
// Runner: node --test. Import con estensione .ts esplicita (loader ESM Node).

import { test } from "node:test";
import assert from "node:assert/strict";
import { humanizeToolResult, formatStepInput } from "./step-detail-logic.ts";

// ── humanizeToolResult ──────────────────────────────────────────────────────

test("humanizeToolResult: content semplice -> testo senza involucro", () => {
  const r = humanizeToolResult(JSON.stringify({ content: "ciao mondo", status: "completed" }));
  assert.equal(r.text, "ciao mondo");
  assert.equal(r.isError, undefined);
});

test("humanizeToolResult: content con \\n -> newline REALI", () => {
  // JSON.stringify produce "app/\nindex.css\nmain.tsx" con newline reali nel
  // valore; il parse li restituisce reali. Verifichiamo anche la variante con
  // "\\n" LETTERALI (escape doppio) che va normalizzata.
  const real = humanizeToolResult(JSON.stringify({ content: "app/\nindex.css\nmain.tsx" }));
  assert.equal(real.text, "app/\nindex.css\nmain.tsx");
  assert.ok(real.text.includes("\n"));

  const literal = humanizeToolResult('{"content":"a\\\\nb"}');
  // Nel raw il content e' la stringa a\nb (backslash-n letterale) -> reso newline.
  assert.equal(literal.text, "a\nb");
});

test("humanizeToolResult: JSON di errore -> isError true", () => {
  const byStatus = humanizeToolResult(JSON.stringify({ content: "Percorso non trovato", status: "error" }));
  assert.equal(byStatus.text, "Percorso non trovato");
  assert.equal(byStatus.isError, true);

  const byErrorField = humanizeToolResult(JSON.stringify({ error: "Percorso non trovato" }));
  assert.equal(byErrorField.text, "Percorso non trovato");
  assert.equal(byErrorField.isError, true);

  const byIsError = humanizeToolResult(JSON.stringify({ content: "boom", is_error: true }));
  assert.equal(byIsError.isError, true);
});

test("humanizeToolResult: stringa non-JSON -> raw invariato", () => {
  const r = humanizeToolResult("[Errore percorso: Percorso non trovato]");
  assert.equal(r.text, "[Errore percorso: Percorso non trovato]");
  assert.equal(r.isError, undefined);
  const plain = humanizeToolResult("semplice testo di output");
  assert.equal(plain.text, "semplice testo di output");
});

test("humanizeToolResult: vuoto -> testo vuoto", () => {
  assert.equal(humanizeToolResult("").text, "");
});

// ── formatStepInput ─────────────────────────────────────────────────────────

test("formatStepInput: stringhe as-is in forma chiave: valore", () => {
  const s = formatStepInput({ path: "src/main.rs" });
  assert.equal(s, "path: src/main.rs");
});

test("formatStepInput: oggetto piatto -> k=v; k2=v2 (niente JSON grezzo)", () => {
  const s = formatStepInput({ opts: { recursive: true, depth: 2 } });
  assert.equal(s, "opts: recursive=true; depth=2");
});

test("formatStepInput: array di primitivi in linea", () => {
  const s = formatStepInput({ paths: ["a.ts", "b.ts"] });
  assert.equal(s, "paths: a.ts, b.ts");
});
