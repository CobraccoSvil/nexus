/**
 * Posizionamento di un popover ANCORATO a un elemento, quando il popover viene
 * reso in un portal (fuori dall'albero del suo ancoraggio).
 *
 * Il difetto che ha reso necessario questo punto: il centro notifiche del run
 * era `position: absolute` dentro la barra di stato, che e' alta 48px e ha
 * `overflow: hidden` per troncare il testo lungo. Un elemento assoluto viene
 * RITAGLIATO dall'antenato che nasconde l'overflow, quindi un pannello alto
 * fino a 380px si vedeva per 48px: il centro notifiche "finiva sotto la chat".
 *
 * Rendere il pannello in un portal lo toglie da quel ritaglio, ma gli toglie
 * anche il riferimento posizionale: le coordinate vanno ricalcolate rispetto al
 * viewport. E' cio' che fa questa funzione, che resta PURA (niente DOM) proprio
 * per poter essere verificata senza un browser.
 *
 * Perche' un portal e non `position: fixed` sul posto: `fixed` oggi
 * funzionerebbe, ma smette di funzionare appena un antenato acquista un
 * `transform` (una qualunque animazione), perche' quello diventa il containing
 * block. Sarebbe una soluzione che regge finche' nessuno tocca il contorno, e
 * si romperebbe in silenzio.
 */

/** Rettangolo dell'ancoraggio nello spazio del viewport (come DOMRect). */
export interface Ancoraggio {
  left: number;
  right: number;
  top: number;
  bottom: number;
}

/** Spazio visibile: le dimensioni del viewport. */
export interface Viewport {
  width: number;
  height: number;
}

/** Ingombro massimo del popover. */
export interface IngombroPopover {
  width: number;
  maxHeight: number;
}

/** Coordinate `fixed` da applicare, piu' il verso in cui il popover si apre. */
export interface PosizionePopover {
  left: number;
  top: number;
  /** Altezza massima concessa DALLO SPAZIO disponibile in quel verso. */
  maxHeight: number;
  verso: "alto" | "basso";
}

/** Distanza fra ancoraggio e popover, e margine minimo dai bordi. */
const STACCO = 6;
const MARGINE = 8;

/**
 * Colloca il popover accanto al suo ancoraggio senza farlo uscire dal viewport.
 *
 * Il verso lo decide lo SPAZIO, non una preferenza fissa: sopra se ci sta, sotto
 * altrimenti, e quando non basta da nessuna parte si sceglie il lato piu'
 * capiente e si restituisce l'altezza che ci entra davvero. Restituire un
 * `maxHeight` piu' grande dello spazio libero produrrebbe di nuovo un pannello
 * tagliato, cioe' lo stesso difetto in una veste diversa.
 *
 * L'allineamento orizzontale segue il bordo DESTRO dell'ancoraggio (il pannello
 * si apre verso sinistra, dove c'e' spazio in una barra che ha i controlli a
 * destra), con rientro se sborderebbe.
 */
export function posizionePopoverAncorato(
  ancora: Ancoraggio,
  viewport: Viewport,
  ingombro: IngombroPopover,
): PosizionePopover {
  const spazioSopra = ancora.top - STACCO - MARGINE;
  const spazioSotto = viewport.height - ancora.bottom - STACCO - MARGINE;

  const staSopra = spazioSopra >= ingombro.maxHeight;
  const staSotto = spazioSotto >= ingombro.maxHeight;
  const verso: "alto" | "basso" = staSopra || (!staSotto && spazioSopra >= spazioSotto)
    ? "alto"
    : "basso";

  const spazioNelVerso = verso === "alto" ? spazioSopra : spazioSotto;
  // Mai negativo: un ancoraggio a ridosso del bordo darebbe un'altezza assurda.
  const maxHeight = Math.max(0, Math.min(ingombro.maxHeight, spazioNelVerso));

  const top =
    verso === "alto" ? ancora.top - STACCO - maxHeight : ancora.bottom + STACCO;

  // Allineato a destra, poi rientrato nei bordi. Il secondo Math.max tiene il
  // popover dentro anche quando e' piu' largo del viewport (schermi stretti):
  // in quel caso vince il bordo sinistro, cosi' resta leggibile da capo.
  const allineatoADestra = ancora.right - ingombro.width;
  const left = Math.max(
    MARGINE,
    Math.min(allineatoADestra, viewport.width - ingombro.width - MARGINE),
  );

  return { left, top, maxHeight, verso };
}
