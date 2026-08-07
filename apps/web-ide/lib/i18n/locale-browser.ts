// Quale lingua l'utente DICHIARA, fra quelle di cui abbiamo un dizionario.
//
// Sta in un modulo suo, separato dal provider React, per due ragioni: e' una
// funzione pura sui tag di lingua e si prova senza montare nulla, e il provider
// non deve conoscere la regola di scelta — la chiama e basta.
//
// IL DIFETTO CHE LO MOTIVA. Il provider partiva da `"en"` e cambiava lingua
// SOLO se l'utente aveva gia' scelto esplicitamente dal selettore, salvandola in
// localStorage. Chi non l'aveva mai aperto vedeva l'inglese qualunque fosse la
// sua lingua — mentre i dizionari `it` e `es` erano completi e presenti.
// Segnalato dall'utente il 06/08/2026 sui banner di risveglio automatico
// («Automatic system wakeup», «A background command failed»), che in italiano
// esistevano gia' parola per parola.

/** I tag che il browser dichiara, in ordine di preferenza dell'utente. */
export type TagDichiarati = readonly string[];

/**
 * La prima lingua dichiarata di cui esiste un dizionario, oppure `null`.
 *
 * L'ordine CONTA ed e' quello dell'utente: `["it-IT", "en-US"]` significa
 * «preferisco l'italiano», e guardare solo il primo elemento darebbe la
 * risposta giusta per caso e quella sbagliata per chi ha una lingua regionale
 * senza dizionario in testa (`["de-CH", "it-IT"]` -> deve dare `it`, non
 * arrendersi al primo).
 *
 * Il confronto e' sul PREFISSO perche' i dizionari sono per lingua e non per
 * regione: `it-IT`, `it-CH` e `it` sono lo stesso dizionario.
 *
 * `null` — e non un ripiego silenzioso — quando nessuna lingua dichiarata e'
 * disponibile: chi chiama deve poter distinguere «l'utente vuole il tedesco che
 * non abbiamo» da «l'utente vuole l'inglese», perche' sono decisioni diverse
 * anche se oggi portano alla stessa schermata.
 */
export function localeDaiTag(
  dichiarati: TagDichiarati,
  disponibili: readonly string[],
): string | null {
  for (const tag of dichiarati) {
    if (typeof tag !== "string") continue;
    const lingua = tag.toLowerCase().split("-")[0];
    if (lingua && disponibili.includes(lingua)) return lingua;
  }
  return null;
}

/**
 * I tag dichiarati da QUESTO browser, nell'ordine giusto.
 *
 * `navigator.languages` e' la lista ordinata; `navigator.language` e' il solo
 * primo elemento ed e' il ripiego per i browser che non espongono la lista.
 * Fuori dal browser (render sul server, test) la lista e' vuota: non si
 * inventa una lingua dove non c'e' nessuno a dichiararla.
 */
export function tagDelBrowser(): TagDichiarati {
  if (typeof navigator === "undefined") return [];
  const lista = navigator.languages;
  if (lista && lista.length > 0) return lista;
  return navigator.language ? [navigator.language] : [];
}
