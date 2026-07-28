"use client";

import { useState, type ChangeEvent, type CSSProperties } from "react";
import { etichettaVisibile, isGroup } from "./auto-width-select-logic";
import type { AutoWidthSelectItem } from "./auto-width-select-logic";

// I tipi vivono nel modulo di logica (node-testable, senza JSX): qui li ri-esporto
// perche' i call site importino tutto da un solo posto.
export type {
  AutoWidthSelectOption,
  AutoWidthSelectGroup,
  AutoWidthSelectItem,
} from "./auto-width-select-logic";

interface AutoWidthSelectProps {
  options: readonly AutoWidthSelectItem[];
  /** Valore controllato. Ometterlo (con o senza defaultValue) rende il select
   *  non controllato: lo stato interno serve solo a misurare il fantasma. */
  value?: string;
  defaultValue?: string;
  onChange?: (value: string, event: ChangeEvent<HTMLSelectElement>) => void;
  /** Stile della pillola: viene applicato IDENTICO al select e al fantasma di
   *  misura, cosi' la larghezza calcolata include padding, bordo e font reali. */
  style: CSSProperties;
  /** Stile del contenitore, per i casi in cui il select viveva dentro un flex
   *  (es. `flexShrink`, `marginLeft`) e il wrapper deve ereditarne il ruolo. */
  wrapperStyle?: CSSProperties;
  title?: string;
  ariaLabel?: string;
  id?: string;
  name?: string;
  disabled?: boolean;
  required?: boolean;
  /** Spazio riservato alla freccia nativa del select, che vive DENTRO il box e
   *  comprimerebbe il testo. Misurato sul browser: 19,8-20,5px a prescindere da
   *  testo, font-size e padding (e' il widget, non il contenuto). 22 = margine. */
  arrowWidth?: number;
  /** Mostra la pillola chiusa nella forma compatta (`shortLabel` dell'opzione
   *  selezionata). La tendina resta per esteso. Lo decide il chiamante, che e'
   *  l'unico a sapere se la sua riga ci sta: qui non si misura niente. */
  breve?: boolean;
}

/**
 * Punto unico (regola L) per i dropdown la cui larghezza deve seguire il testo.
 *
 * Un `<select>` nativo si dimensiona sull'opzione PIU' LUNGA della lista, non su
 * quella selezionata: la pillola chiusa resta larga anche quando mostra due
 * caratteri. Qui la larghezza la detta un fantasma non visibile che contiene
 * solo l'etichetta selezionata; il select ci sta sopra in overlay. La tendina
 * aperta la disegna il browser e continua ad allargarsi sul contenuto delle
 * option, quindi le etichette lunghe restano leggibili.
 *
 * Il select e' `position: absolute` per una ragione precisa, verificata sul
 * browser e non deducibile a occhio: in flusso normale (griglia sovrapposta,
 * overlay in-flow) un `width: 100%` percentuale NON toglie il select dal calcolo
 * della larghezza intrinseca del contenitore, che torna a essere quella
 * dell'opzione piu' lunga e annulla l'intero effetto. Solo l'uscita dal flusso
 * lascia decidere al fantasma.
 *
 * Il fantasma e' la stessa misura del sistema, non una stima: eredita lo stesso
 * `style` del select, quindi non puo' divergere dal font o dal padding reale
 * (regola O).
 *
 * NON va usato dove la larghezza e' imposta dal layout (`width: 100%`, `flex: 1`
 * nei campi di un form): li' la lista non detta niente e restringere il campo
 * romperebbe l'allineamento delle colonne.
 */
export function AutoWidthSelect({
  options,
  value,
  defaultValue,
  onChange,
  style,
  wrapperStyle,
  title,
  ariaLabel,
  id,
  name,
  disabled,
  required,
  arrowWidth = 22,
  breve = false,
}: AutoWidthSelectProps) {
  const controllato = value !== undefined;
  const [valoreInterno, setValoreInterno] = useState(defaultValue);
  const corrente = controllato ? value : valoreInterno;

  const etichetta = etichettaVisibile(options, corrente, breve);

  const handleChange = (event: ChangeEvent<HTMLSelectElement>) => {
    if (!controllato) setValoreInterno(event.target.value);
    onChange?.(event.target.value, event);
  };

  return (
    <span
      style={{
        position: "relative",
        display: "inline-block",
        minWidth: 0,
        flexShrink: 0,
        ...wrapperStyle,
      }}
    >
      <span
        aria-hidden="true"
        style={{
          ...style,
          boxSizing: "border-box",
          display: "block",
          whiteSpace: "nowrap",
          // In forma compatta il fantasma smette di essere solo un metro e diventa
          // cio' che si vede: disegna lui la pillola con il pittogramma, e il
          // select gli sta sopra invisibile (vedi sotto). Un <select> nativo mostra
          // il testo dell'<option> selezionata e non c'e' modo di dargliene uno
          // diverso senza toccare la tendina: senza questo scambio la pillola
          // stretta conterrebbe "Automatico" tagliato a meta'.
          visibility: breve ? "visible" : "hidden",
          pointerEvents: "none",
          // Senza questo, con un maxWidth nello style il fantasma resta capato
          // alla larghezza giusta ma il TESTO trabocca fuori dal suo box, e il
          // trabocco entra nello scrollWidth di ogni antenato. Il select nativo,
          // essendo un widget, si e' sempre tagliato da solo: chi misura
          // scrollWidth per decidere un layout (ChatHead) leggerebbe centinaia di
          // px inesistenti. Misurato: cap 210 con etichetta lunga, riga a 460
          // contro i 250 del select nativo; con overflow hidden torna a 250.
          // Senza maxWidth e' un no-op (il fantasma si dimensiona su max-content).
          overflow: "hidden",
          // Il fantasma detta anche l'altezza della pillola: con line-height
          // "normal" un'etichetta che contiene un pittogramma alzerebbe la riga e
          // le pillole non sarebbero piu' alte uguali fra loro. Chi ha bisogno di
          // combaciare con campi vicini puo' imporre il proprio nello style.
          lineHeight: style.lineHeight ?? 1.4,
        }}
      >
        {etichetta}
        <span style={{ display: "inline-block", width: breve ? 0 : arrowWidth }} />
      </span>
      <select
        value={controllato ? value : undefined}
        defaultValue={controllato ? undefined : defaultValue}
        onChange={handleChange}
        title={title}
        aria-label={ariaLabel}
        id={id}
        name={name}
        disabled={disabled}
        required={required}
        style={{
          ...style,
          boxSizing: "border-box",
          position: "absolute",
          left: 0,
          top: 0,
          width: "100%",
          height: "100%",
          // Compatto: il select resta l'elemento vero (focus, tastiera, tendina
          // nativa, screen reader) ma smette di dipingere — la pillola sotto e' gia'
          // disegnata dal fantasma. `opacity` e non `visibility: hidden`, che lo
          // toglierebbe dal focus e dal puntatore. La tendina aperta la disegna il
          // sistema operativo e resta visibile e per esteso.
          ...(breve ? { opacity: 0 } : {}),
        }}
      >
        {options.map((item) =>
          isGroup(item) ? (
            <optgroup key={item.label} label={item.label}>
              {item.options.map((option) => (
                <option key={option.value} value={option.value} disabled={option.disabled}>
                  {option.label}
                </option>
              ))}
            </optgroup>
          ) : (
            <option key={item.value} value={item.value} disabled={item.disabled}>
              {item.label}
            </option>
          ),
        )}
      </select>
    </span>
  );
}
