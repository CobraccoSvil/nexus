// Unit test dell'aggancio dei sub-run (node --test, type-stripping).
//
// I payload qui sotto sono nella forma che produce
// `crates/mcp-core/src/agent_tools/subagent_native.rs`: chiusura riuscita
// (`finalize_success`, riga ~4232), timeout (~4144), background (~3318) e batch
// (~602). La chiave e' `subagent_run_id` (`const K_SUB_RUN_ID`, riga 67).
import { test } from "node:test";
import assert from "node:assert/strict";
import { childRunIdsFromToolResult } from "./subagent-runs.ts";

const RUN_A = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
const RUN_B = "7c9e6679-7425-40de-944b-e07fc1f90ae7";

test("dispatch_subagent: id dal campo strutturato", () => {
  const result = JSON.stringify({
    subagent_run_id: RUN_A,
    kind: "coder",
    status: "completed",
    summary: "fatto",
    iterations: 3,
  });
  assert.deepEqual(childRunIdsFromToolResult("dispatch_subagent", result), [RUN_A]);
});

test("dispatch_subagents: un id per ogni figlio del batch", () => {
  const result = JSON.stringify({
    count: 2,
    ok: 2,
    failed: 0,
    results: [
      { subagent_run_id: RUN_A, kind: "coder", status: "completed" },
      { subagent_run_id: RUN_B, kind: "reviewer", status: "completed" },
    ],
  });
  assert.deepEqual(childRunIdsFromToolResult("dispatch_subagents", result), [RUN_A, RUN_B]);
});

test("batch in background: child_run_ids, senza duplicare i results", () => {
  const result = JSON.stringify({
    count: 1,
    ok: 1,
    failed: 0,
    background_dispatched: true,
    child_run_ids: [RUN_A],
    results: [{ background_dispatched: true, subagent_run_id: RUN_A, status: "running" }],
  });
  assert.deepEqual(childRunIdsFromToolResult("dispatch_subagents", result), [RUN_A]);
});

test("timeout: il figlio si aggancia comunque", () => {
  const result = JSON.stringify({
    subagent_run_id: RUN_A,
    kind: "coder",
    status: "timeout",
    error: "[Sub-agent timeout]",
  });
  assert.deepEqual(childRunIdsFromToolResult("dispatch_subagent", result), [RUN_A]);
});

test("dispatch_subtask non aggancia nulla: quel tool non esiste piu'", () => {
  const result = JSON.stringify({ subagent_run_id: RUN_A });
  assert.deepEqual(childRunIdsFromToolResult("dispatch_subtask", result), []);
});

test("altri tool ignorati", () => {
  const result = JSON.stringify({ subagent_run_id: RUN_A });
  assert.deepEqual(childRunIdsFromToolResult("write_file", result), []);
});

test("errore in prosa del tool: nessun sub-run, nessuna eccezione", () => {
  // `err()` del backend risponde con testo, non JSON.
  const prosa = "❌ [dispatch_subagent] parametro 'kind' obbligatorio";
  assert.deepEqual(childRunIdsFromToolResult("dispatch_subagent", prosa), []);
});

test("il testo del messaggio NON e' piu' una fonte di id", () => {
  // Esattamente la forma che la vecchia regex /ID:\s*([0-9a-f-]{36})/i coglieva:
  // se il backend smettesse di dichiarare il campo, la chat deve accorgersene
  // (nessun aggancio) invece di reggersi sul wording.
  const prosa = `Sub-agente avviato. ID: ${RUN_A}`;
  assert.deepEqual(childRunIdsFromToolResult("dispatch_subagent", prosa), []);
});

test("input assenti o malformati", () => {
  assert.deepEqual(childRunIdsFromToolResult(null, null), []);
  assert.deepEqual(childRunIdsFromToolResult("dispatch_subagent", ""), []);
  assert.deepEqual(childRunIdsFromToolResult("dispatch_subagent", "null"), []);
  assert.deepEqual(childRunIdsFromToolResult("dispatch_subagent", "[1,2]"), []);
  // Campo presente ma non un UUID: scartato, niente sottoscrizione fantasma.
  assert.deepEqual(
    childRunIdsFromToolResult("dispatch_subagent", JSON.stringify({ subagent_run_id: "auto" })),
    [],
  );
});
