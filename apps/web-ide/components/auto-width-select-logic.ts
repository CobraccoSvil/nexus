export interface AutoWidthSelectOption {
  value: string;
  label: string;
  /** Etichetta della PILLOLA CHIUSA quando lo spazio in riga non basta (di solito
   *  un pittogramma). La tendina aperta continua a mostrare `label`: chi apre il
   *  menu deve poter leggere per esteso cosa sta scegliendo, e' solo la vetrina
   *  che si stringe. Assente = la pillola mostra `label` a qualunque larghezza. */
  shortLabel?: string;
  disabled?: boolean;
}

export interface AutoWidthSelectGroup {
  label: string;
  options: readonly AutoWidthSelectOption[];
}

export type AutoWidthSelectItem = AutoWidthSelectOption | AutoWidthSelectGroup;

export function isGroup(item: AutoWidthSelectItem): item is AutoWidthSelectGroup {
  return "options" in item;
}

export function flatten(items: readonly AutoWidthSelectItem[]): AutoWidthSelectOption[] {
  return items.flatMap((item) => (isGroup(item) ? [...item.options] : [item]));
}

/**
 * Etichetta che il select mostra davvero, e quindi l'unica che il fantasma deve
 * misurare.
 *
 * Il fallback non e' una comodita': quando il valore non corrisponde a nessuna
 * opzione, un <select> nativo mostra comunque la PRIMA. Misurare il valore
 * orfano darebbe una pillola larga quanto una stringa che nessuno vede.
 *
 * `breve` chiede la forma compatta (vedi `shortLabel`). Chi non l'ha dichiarata
 * resta col suo `label`: e' una scelta per-opzione, non una troncatura d'ufficio
 * — tagliare a N caratteri produrrebbe "Automat…" invece di un pittogramma.
 */
export function etichettaVisibile(
  items: readonly AutoWidthSelectItem[],
  value: string | undefined,
  breve = false,
): string {
  const piatte = flatten(items);
  const selezionata = piatte.find((option) => option.value === value) ?? piatte[0];
  if (!selezionata) return "";
  return breve ? selezionata.shortLabel ?? selezionata.label : selezionata.label;
}
