// Logica pura per il dimensionamento orizzontale dello shell dell'IDE.
//
// Punto unico (regola L): la matematica min/max/clamp delle larghezze vive qui —
// activity bar, sidebar primaria, spazio residuo per l'area centrale e vincoli
// del pannello a larghezza fissa. `ide-shell.tsx` delega a queste funzioni invece
// di ripetere inline i vincoli, cosi' l'unico posto in cui cambiare le soglie e'
// questo modulo (testato in panel-sizing-logic.test.ts).
//
// Le larghezze della chrome stanno qui e non in `ide-shell.tsx` per una ragione
// precisa: i vincoli del pannello dipendono da quanto spazio la chrome ha gia'
// consumato. Se il test le ricalcolasse a mano misurerebbe la propria aritmetica
// invece della cascata vera (regola O).
//
// Runner test: node --test.

export interface PanelBounds {
  min: number;
  max: number;
}

// Soglie viewport condivise con l'header/sidebar dell'IDE.
const NARROW_VIEWPORT = 1280;
const MOBILE_VIEWPORT = 980;

// Chrome che precede l'area centrale e ne consuma la larghezza.
export function activityBarWidth(viewportWidth: number): number {
  return viewportWidth < MOBILE_VIEWPORT ? 46 : 52;
}

export function leftSidebarBounds(viewportWidth: number): PanelBounds {
  const min =
    viewportWidth < MOBILE_VIEWPORT ? 160 : viewportWidth < NARROW_VIEWPORT ? 190 : 220;
  const max = Math.max(min, Math.min(520, Math.floor(viewportWidth * 0.46)));
  return { min, max };
}

export function clampLeftWidth(width: number, viewportWidth: number): number {
  const { min, max } = leftSidebarBounds(viewportWidth);
  return Math.max(min, Math.min(max, width));
}

// Spazio che le colonne dell'area centrale si dividono davvero: il viewport meno
// la chrome che lo precede. E' l'input onesto dei vincoli del pannello.
export function mainAreaAvailableWidth(
  viewportWidth: number,
  leftWidth: number,
  primarySidebarVisible: boolean,
): number {
  const left = primarySidebarVisible ? clampLeftWidth(leftWidth, viewportWidth) : 0;
  return Math.max(0, viewportWidth - activityBarWidth(viewportWidth) - left);
}

// Minimo della colonna a larghezza fissa: invariato rispetto al comportamento
// storico del pannello destro.
function fixedPanelMinWidth(viewportWidth: number): number {
  return viewportWidth < MOBILE_VIEWPORT ? 240 : 280;
}

// Minimo della colonna flessibile — la chat in ai-center/split-ai-editor,
// l'editor in editor-center.
//
// MISURATO sul DOM vivo, non stimato: sotto i 280px il gruppo di destra
// dell'header della chat collassa a larghezza 0 e select e bottoni gli sfondano
// fuori; a 311 e 320 i bottoni "+ / rinomina / elimina" restano comunque tagliati
// oltre il bordo destro; da 340px la chat rende pulita (scrollWidth == box, zero
// elementi fuori). E' il minimo delle due colonne che vale la pena difendere:
// e' piu' alto di quello della colonna fissa perche' e' misurato sul contenuto
// piu' esigente, non perche' la colonna sia piu' importante.
const FLEXIBLE_PANEL_MIN = 340;

// Vincoli larghezza della colonna a larghezza fissa dell'area centrale.
//
// `availableWidth` e' lo spazio che le due colonne si dividono davvero: il
// viewport MENO la chrome che lo precede (activity bar + sidebar primaria).
// Il solo viewport non basta a decidere: a 375px restano 169px utili, ma il
// pavimento assoluto ne pretendeva 240 e la colonna `minmax(0, 1fr)` accanto
// veniva schiacciata a 0 (misurato sul DOM: box 0px, 29 elementi che sfondavano).
//
// Sotto la soglia "narrow" il tetto si allarga (percentuale e cap assoluto piu'
// generosi): su viewport stretti l'utente ha bisogno di piu' spazio orizzontale
// per la chat/AI, altrimenti il 60% clampato a 620px lascia il pannello troppo
// angusto. Il cap assoluto resta comunque ragionevole per non mangiare tutto lo
// schermo.
export function rightSidebarBounds(viewportWidth: number, availableWidth: number): PanelBounds {
  const min = fixedPanelMinWidth(viewportWidth);

  // Due colonne affiancate costano i due minimi. Sotto, non ci stanno entrambe:
  // la colonna fissa cede TUTTO lo spazio e resta un pannello solo. Qui sta il
  // punto critico — cedere prima renderebbe angusti due pannelli che ci stavano,
  // cedere dopo ne schiaccia uno a zero.
  if (availableWidth < min + FLEXIBLE_PANEL_MIN) {
    return { min: 0, max: 0 };
  }

  const isNarrow = viewportWidth < NARROW_VIEWPORT;
  const pct = isNarrow ? 0.72 : 0.6;
  const absoluteCap = isNarrow ? 760 : 620;
  // Il tetto non puo' mangiarsi il minimo della colonna flessibile: senza questo
  // vincolo bastava trascinare il divisorio per riprodurre lo stesso collasso.
  const roomCap = availableWidth - FLEXIBLE_PANEL_MIN;
  // `max >= min` e' garantito senza bisogno di un Math.max che lo maschererebbe:
  // roomCap >= min vale per il ramo sopra, e sia il cap assoluto sia
  // viewportWidth * pct superano il min in ogni regime raggiungibile. Il test
  // "max non scende mai sotto il min" lo verifica su tutto lo spettro.
  const max = Math.min(absoluteCap, Math.floor(viewportWidth * pct), roomCap);
  return { min, max };
}

// Riporta una larghezza richiesta dentro i vincoli correnti.
export function clampRightWidth(
  width: number,
  viewportWidth: number,
  availableWidth: number,
): number {
  const { min, max } = rightSidebarBounds(viewportWidth, availableWidth);
  return Math.max(min, Math.min(max, width));
}

// Una riga di controlli ci sta per intero nello spazio che ha, o deve passare
// alla sua forma raccolta? Due chiamanti oggi, stessa domanda: la testata della
// chat (riga distesa <-> popover hamburger) e la barra del composer (etichette
// per esteso <-> sole icone). E' la REGOLA, non la misura: il nome resta
// generico perche' la domanda non ha niente di specifico all'uno o all'altro
// (regola L: un secondo confronto copiato-e-adattato divergerebbe).
//
// NON e' una soglia fissa in px, e per una ragione precisa: la larghezza naturale
// della riga dipende da cosa contiene — il nome del profilo e il titolo della
// sessione per la testata, la lingua e i controlli condizionali (pin, modello)
// per il composer. Una costante calibrata su un caso corto direbbe "ci sta"
// mentre un caso lungo sfonda, e viceversa. Percio' il chiamante MISURA sul DOM
// vivo la larghezza naturale della riga renderizzata (scrollWidth, non vincolata) e
// lo spazio disponibile (clientWidth dell'host), e passa i due numeri qui (regola O:
// la decisione nasce dalla misura del rendering vero, non da una stima). Questo
// modulo resta il punto unico (regola L) della REGOLA di confronto.
//
// `naturalWidth` e' sempre la larghezza della forma DISTESA, anche quando si e'
// gia' raccolti: e' la sola che risponda a "se mi distendo, ci sto?". Passare la
// larghezza della forma raccolta (piu' stretta per costruzione) darebbe sempre
// "ci sta", cioe' un rientro immediato e poi un nuovo collasso — l'oscillazione
// che l'isteresi qui sotto esiste per impedire.
//
// La banda morta ROW_REENTRY_GUARD da' quell'isteresi: senza, a cavallo del confine
// un pixel di ResizeObserver farebbe oscillare disteso<->raccolto a ogni frame.
export const ROW_REENTRY_GUARD = 12;

export function rowFitsInline(
  availableWidth: number,
  naturalWidth: number,
  currentlyInline: boolean,
): boolean {
  // Non ancora misurato: parti dalla riga (default ottimista). Il primo
  // ResizeObserver, in useLayoutEffect, corregge prima del paint.
  if (naturalWidth <= 0 || availableWidth <= 0) return true;
  // In riga si resta finche' la riga non sfonda davvero lo spazio disponibile.
  if (currentlyInline) return naturalWidth <= availableWidth;
  // Gia' raccolta: si torna distesi solo con un margine, per non ri-collassare al
  // primo pixel guadagnato.
  return naturalWidth + ROW_REENTRY_GUARD <= availableWidth;
}
