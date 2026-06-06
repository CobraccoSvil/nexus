/**
 * Rimozione delle sequenze di escape ANSI/CSI da testo grezzo.
 *
 * Punto unico (regola L) per lo stripping ANSI nel web-ide: i pannelli che
 * mostrano output di processi (Playwright, output build/test, terminale) devono
 * delegare qui invece di re-implementare regex parziali. Alcune copie sparse
 * strippavano solo i colori SGR (ESC-[-...-m) lasciando visibili come testo
 * grezzo le sequenze di cursore ed erase (ESC-[1A, ESC-[2K): e' esattamente cio'
 * che rendeva illeggibile l'output dei test Playwright.
 *
 * Ogni pattern inizia con \x1b (ESC). Senza, si rimuoverebbero parentesi quadre
 * e caratteri di testo normale (es. tutte le lettere maiuscole).
 */

// CSI: ESC '[' + parametri (0x30-0x3f) + byte intermedi (0x20-0x2f) + byte finale
// (0x40-0x7e). Copre SGR colori (m), cursore (A-H), erase (J, K), ecc.
const ANSI_CSI_RE = /\x1b\[[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]/g;
// OSC: ESC ']' ... terminato da BEL (\x07) o ST (ESC '\').
const ANSI_OSC_RE = /\x1b\][\s\S]*?(?:\x07|\x1b\\)/g;
// Altre sequenze ESC a singolo byte finale (es. ESC c = reset terminale).
const ANSI_SINGLE_RE = /\x1b[@-Z\\\]^_]/g;

/** Ritorna `text` senza sequenze di escape ANSI/CSI/OSC, leggibile come testo. */
export function stripAnsi(text: string): string {
  if (!text) return text;
  return text
    .replace(ANSI_OSC_RE, "")
    .replace(ANSI_CSI_RE, "")
    .replace(ANSI_SINGLE_RE, "");
}
