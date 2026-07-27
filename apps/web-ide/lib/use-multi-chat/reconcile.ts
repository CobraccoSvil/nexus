/**
 * Riconciliazione tab chat persistite vs sessioni reali del progetto: punto
 * unico (regola L) usato dal bootstrap di useMultiChat a ogni cambio progetto.
 *
 * Regole:
 * - le tab persistite che non corrispondono piu' a una sessione del progetto
 *   vengono scartate (localStorage stantio, sessione cancellata altrove);
 * - se non sopravvive nessuna tab, si apre la prima sessione disponibile;
 * - l'active persistito vale solo se punta a una sessione esistente E ancora
 *   aperta come tab; in mancanza si attiva l'ultima tab aperta;
 * - progetto senza sessioni: nessuna tab, nessuna attiva (il chiamante decide
 *   se crearne una).
 */
export function reconcileSessionTabs(
  sessionIds: readonly string[],
  persistedTabs: readonly string[],
  persistedActive: string | null,
): { tabs: string[]; active: string | null } {
  const valid = new Set(sessionIds);
  let tabs = persistedTabs.filter((id) => valid.has(id));
  if (tabs.length === 0 && sessionIds.length > 0) {
    tabs = [sessionIds[0]];
  }
  const active =
    persistedActive && tabs.includes(persistedActive)
      ? persistedActive
      : tabs[tabs.length - 1] ?? null;
  return { tabs, active };
}
