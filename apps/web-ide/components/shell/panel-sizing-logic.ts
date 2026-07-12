// Logica pura per il dimensionamento del pannello destro (AI workspace) dell'IDE.
//
// Punto unico (regola L): la matematica min/max/clamp della larghezza del
// pannello destro vive qui. `ide-shell.tsx` delega a queste funzioni invece di
// ripetere inline i vincoli, cosi' l'unico posto in cui cambiare le soglie e'
// questo modulo (testato in panel-sizing-logic.test.ts).
//
// Runner test: node --test.

export interface PanelBounds {
  min: number;
  max: number;
}

// Soglie viewport condivise con l'header/sidebar dell'IDE.
const NARROW_VIEWPORT = 1280;
const MOBILE_VIEWPORT = 980;

// Vincoli larghezza pannello destro in funzione della larghezza del viewport.
//
// Sotto la soglia "narrow" il tetto si allarga (percentuale e cap assoluto piu'
// generosi): su viewport stretti l'utente ha bisogno di piu' spazio orizzontale
// per la chat/AI, altrimenti il 60% clampato a 620px lascia il pannello troppo
// angusto. Il cap assoluto resta comunque ragionevole per non mangiare tutto lo
// schermo.
export function rightSidebarBounds(viewportWidth: number): PanelBounds {
  const isNarrow = viewportWidth < NARROW_VIEWPORT;
  const isMobile = viewportWidth < MOBILE_VIEWPORT;
  const min = isMobile ? 240 : 280;
  const pct = isNarrow ? 0.72 : 0.6;
  const absoluteCap = isNarrow ? 760 : 620;
  const max = Math.max(min, Math.min(absoluteCap, Math.floor(viewportWidth * pct)));
  return { min, max };
}

// Riporta una larghezza richiesta dentro i vincoli correnti del viewport.
export function clampRightWidth(width: number, viewportWidth: number): number {
  const { min, max } = rightSidebarBounds(viewportWidth);
  return Math.max(min, Math.min(max, width));
}
