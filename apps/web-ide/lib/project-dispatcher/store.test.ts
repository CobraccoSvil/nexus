// Unit test dello store del dispatcher.
// Runner: `node --test` con type-stripping nativo. Import con estensione .ts
// esplicita, richiesta dal loader ESM di Node.

import { test } from "node:test";
import assert from "node:assert/strict";
import { selectTodoStatuses, useProjectStore } from "./store.ts";
import type { EnvelopedEvent } from "./types.ts";

function todoUpdated(seq: number, todoId: string, status: string): EnvelopedEvent {
  return {
    seq,
    ts: 1_700_000_000_000 + seq,
    payload: { kind: "TodoUpdated", run_id: "run-1", todo_id: todoId, status },
  } as EnvelopedEvent;
}

// REGRESSIONE: i marker della checklist del piano restavano a [ ] durante il run
// e comparivano corretti solo dopo un refresh. La causa non era la consegna
// dell'evento (arriva, ed e' sempre arrivato) ma DOVE finiva: la checklist lo
// accumulava in uno stato locale, e i suoi renderer si smontano di continuo
// mentre il run procede. Senza replay nella consegna, ogni evento ricevuto a
// componente smontato era perso per sempre.
test("TodoUpdated lascia lo stato nello store, non nel componente", () => {
  useProjectStore.getState().applyEvent(todoUpdated(1, "todo-a", "in_progress"));
  useProjectStore.getState().applyEvent(todoUpdated(2, "todo-b", "completed"));

  const statuses = selectTodoStatuses(useProjectStore.getState());
  assert.equal(statuses["todo-a"], "in_progress");
  assert.equal(statuses["todo-b"], "completed");
});

test("l'ultimo stato di un todo vince sui precedenti", () => {
  useProjectStore.getState().applyEvent(todoUpdated(10, "todo-c", "pending"));
  useProjectStore.getState().applyEvent(todoUpdated(11, "todo-c", "in_progress"));
  useProjectStore.getState().applyEvent(todoUpdated(12, "todo-c", "completed"));

  assert.equal(selectTodoStatuses(useProjectStore.getState())["todo-c"], "completed");
});

// Lo store e' l'unico posto dove lo stato sopravvive: il valore deve restare
// leggibile anche molto dopo l'arrivo dell'evento, quando i componenti che
// c'erano in quel momento sono stati smontati e ricreati piu' volte.
test("lo stato dei todo sopravvive agli eventi successivi di altro tipo", () => {
  useProjectStore.getState().applyEvent(todoUpdated(20, "todo-d", "completed"));
  useProjectStore.getState().applyEvent({
    seq: 21,
    ts: 1_700_000_000_021,
    payload: { kind: "FileChanged", path: "src/main.rs", op: "modified" },
  } as EnvelopedEvent);

  assert.equal(
    selectTodoStatuses(useProjectStore.getState())["todo-d"],
    "completed",
    "un evento non correlato non deve azzerare l'avanzamento del piano",
  );
});
