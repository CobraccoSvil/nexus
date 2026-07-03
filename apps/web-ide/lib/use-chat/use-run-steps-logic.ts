// Logica PURA del lazy-fetch degli step storici (ADR 0037), senza dipendenze da
// React o dal client API: cosi' e' testabile con `node --test` senza renderer.
// L'hook useResolvedRunSteps (use-run-steps.ts) delega a questa funzione.

/**
 * Decide se serve il lazy-fetch degli step dal DB. Fetch necessario quando: non
 * ci sono step gia' presenti, il fetch e' abilitato e c'e' un runId. Punto unico
 * della regola di fetch (regola L). Type guard su `runId`: quando ritorna true
 * il chiamante ha `runId` ristretto a `string` (nessun cast).
 */
export function shouldFetchRunSteps(
  hasPresentSteps: boolean,
  enabled: boolean,
  runId: string | undefined,
): runId is string {
  return !hasPresentSteps && enabled && Boolean(runId);
}
