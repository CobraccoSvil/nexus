import type { CSSProperties } from "react";

/** I colori che i controlli della barra utente leggono dal tema. */
export interface ColoriBarraUtente {
  border: string;
  text: string;
  textMuted: string;
  accentBg: string;
}

/** Gli stili dei tre controlli in coda alla barra utente. */
export interface StiliBarraUtente {
  /** Link all'area Admin, mostrato fuori da /admin. */
  admin: CSSProperties;
  /** Link all'IDE, mostrato dentro /admin. */
  ide: CSSProperties;
  /** Pulsante di uscita, sempre presente. */
  uscita: CSSProperties;
}

/**
 * Scatola condivisa dei controlli della barra utente.
 *
 * NON dichiara `width`, e non e' una svista: il contenuto di questi controlli e'
 * TESTO, e per l'uscita e' testo TRADOTTO (`auth.logout` vale "Esci", "Logout",
 * "Cerrar sesion"). La larghezza e' percio' una proprieta' della stringa, che il
 * codice non conosce e non puo' fissare -- cambia con la lingua per definizione.
 *
 * Il `26` che stava qui veniva dai pulsanti-ICONA della stessa interfaccia (il
 * menu compatto, la chiusura del drawer, le sigle della sidebar), dove il
 * quadrato E' la forma perche' il contenuto e' un glifo di larghezza nota. Su
 * un'etichetta quel vincolo taglia il testo a destra: misurato il 02/08/2026 su
 * /admin, "Esci" a 13px eccede i 26px del riquadro, e "Admin" -- stesso vincolo,
 * visibile fuori dall'area admin -- lo eccede di piu'.
 *
 * Il quadrato resta come MINIMO, non come misura: `minWidth` tiene il bersaglio
 * cliccabile senza impedire al controllo di crescere quanto serve.
 */
const SCATOLA: CSSProperties = {
  height: 26,
  minWidth: 26,
  padding: "0 8px",
  boxSizing: "border-box",
  display: "inline-flex",
  alignItems: "center",
  justifyContent: "center",
  borderRadius: 5,
  whiteSpace: "nowrap",
  textDecoration: "none",
};

/**
 * Punto unico della forma dei controlli della barra utente.
 *
 * I tre controlli la ricevono da qui invece di ricopiarla: prima ognuno portava
 * la propria copia, e le copie erano gia' divergenti -- il link IDE aveva la
 * forma giusta (altezza piu' padding), gli altri due un riquadro fisso. Con un
 * punto solo, il vincolo di larghezza non puo' tornare per copia in uno dei tre.
 */
export function stiliBarraUtente(colori: ColoriBarraUtente): StiliBarraUtente {
  return {
    admin: {
      ...SCATOLA,
      background: colori.accentBg,
      color: colori.text,
      fontSize: 13,
      fontWeight: 600,
    },
    ide: {
      ...SCATOLA,
      border: `1px solid ${colori.border}`,
      background: "transparent",
      color: colori.text,
      fontSize: 11,
      fontWeight: 700,
      letterSpacing: "0.04em",
    },
    uscita: {
      ...SCATOLA,
      border: `1px solid ${colori.border}`,
      background: "transparent",
      color: colori.textMuted,
      fontSize: 13,
      cursor: "pointer",
    },
  };
}
