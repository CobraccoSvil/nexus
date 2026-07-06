// Punto unico (regola L) del segnale "il backend e' tornato disponibile".
//
// Problema che risolve: quando mcp-core si riavvia (dev-stop+dev-start, deploy)
// con la pagina aperta, i canali SSE (agent-stream per-run e dispatcher globale)
// cadono. Ognuno ha il proprio reconnect, ma NESSUNO ha un trigger esplicito di
// "backend ritornato" che risincronizzi lo stato dal server: la UI resta cieca
// sul run in corso finche' l'utente non fa un refresh manuale.
//
// Questo modulo polla /api/health e, sulla transizione down->up, notifica i
// consumer registrati (canale run in use-chat, dispatcher globale in
// connection.ts) che devono ri-verificare lo stato corrente e riagganciare gli
// stream. Un solo timer condiviso: niente poll duplicati per canale.
//
// Readiness, non solo liveness (regola G / falso "up"): mcp-core espone /health
// 200 all'avvio ma la routing matrix ha un retry-loop di ~25s; consideriamo il
// backend "up" solo quando components.neural_core === true, cosi' non emettiamo
// un recovery prematuro verso endpoint che risponderebbero ancora 503.

import { getHealth } from "./system";

type BackendState = "unknown" | "up" | "down";

const POLL_UP_MS = 5000; // backend sano: check di liveness rilassato
const POLL_DOWN_MS = 2000; // backend giu': check piu' frequente per riagganciare presto

let state: BackendState = "unknown";
let timer: ReturnType<typeof setTimeout> | null = null;
let started = false;
let inFlight = false;
const listeners = new Set<() => void>();

async function probe(): Promise<boolean> {
  try {
    const health = await getHealth();
    // "up" solo se il Core e' realmente operativo (readiness), non solo se
    // l'endpoint risponde. Se il campo manca (versione backend diversa) si
    // ricade sullo status generale.
    if (typeof health.components?.neural_core === "boolean") {
      return health.components.neural_core;
    }
    return health.status === "ok" || health.status === "healthy";
  } catch {
    return false;
  }
}

function schedule(delayMs: number): void {
  if (timer) clearTimeout(timer);
  timer = setTimeout(() => void tick(), delayMs);
}

async function tick(): Promise<void> {
  if (inFlight) {
    schedule(POLL_DOWN_MS);
    return;
  }
  inFlight = true;
  let up = false;
  try {
    up = await probe();
  } finally {
    inFlight = false;
  }

  const prev = state;
  state = up ? "up" : "down";

  // Emette recovery SOLO sulla transizione down->up (non al primo giro
  // unknown->up, che non e' un ritorno ma lo stato iniziale). Le callback sono
  // isolate: un errore in una non deve impedire alle altre di girare.
  if (prev === "down" && state === "up") {
    for (const cb of Array.from(listeners)) {
      try {
        cb();
      } catch {
        // best-effort: il canale che fallisce riprovera' al prossimo recovery.
      }
    }
  }

  schedule(state === "up" ? POLL_UP_MS : POLL_DOWN_MS);
}

/** Avvia il monitor (idempotente: piu' chiamate non creano timer duplicati).
 *  Da invocare al mount dell'app / del dispatcher. */
export function startBackendHealthMonitor(): void {
  if (started || typeof window === "undefined") return;
  started = true;
  void tick();
}

/** Registra un listener invocato a ogni transizione backend down->up.
 *  Ritorna la funzione di unsubscribe. Avvia il monitor se non gia' attivo. */
export function onBackendRecovered(cb: () => void): () => void {
  listeners.add(cb);
  startBackendHealthMonitor();
  return () => {
    listeners.delete(cb);
  };
}

/** Stato corrente osservato dal monitor (per UI/diagnostica). */
export function backendState(): BackendState {
  return state;
}
