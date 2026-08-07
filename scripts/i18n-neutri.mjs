// Testo che NON e' lingua, e che quindi non si traduce ne' si conta come debito.
//
// PUNTO UNICO (regola L) fra i due strumenti i18n: `i18n-estrai.mjs` la usa per
// non proporre queste voci alla traduzione, `i18n-ratchet.mjs` per non contarle
// come debito. Con due liste separate i due avrebbero avuto due idee diverse di
// cosa sia «testo», e il ratchet avrebbe segnalato per sempre voci che
// l'estrattore si rifiuta di estrarre — un debito che nessuno puo' ripagare.
//
// La lista e' ESPLICITA e non un'euristica sulle maiuscole: fra le maiuscole ci
// sono anche parole vere — ESAURITO, NON IMPOSTATO, DISABILITATO — che vanno
// tradotte, e infatti lo sono (`badge.*` nei dizionari).
export const NEUTRI = new Set([
  // Sigle tecniche.
  "AI", "API", "URL", "ID", "SQL", "DB", "UTF-8", "JSON", "HTTP", "HTTPS",
  "IDE", "CPU%", "MEM%", "IMG", "BIN", "TXT", "LF", "CRLF",
  // Nome del prodotto e marchi: identici in ogni lingua.
  "Nexus", "NEXUS", "GitHub", "Git", "Redis", "PostgreSQL", "MySQL", "SQLite",
  // Tipi e identificatori che l'euristica puo' scambiare per testo.
  "Promise", "Boolean", "String",
  // Esempi di codice usati come placeholder: tradurli li renderebbe sbagliati.
  "PORT=20000", "CREATE TABLE ...", "localhost",
]);
