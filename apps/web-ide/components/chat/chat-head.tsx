"use client";

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { chatHeadFitsInline } from "../shell/panel-sizing-logic";
import { ChatHeadInline } from "./chat-head-inline";
import { ChatHeadPopover, type ChatHeadPopoverProps } from "./chat-head-popover";

/**
 * Testata della chat nell'header dell'AI Workspace: distesa in riga quando i
 * controlli ci stanno, raccolta nel popover a comparsa (l'hamburger) quando non
 * ci stanno.
 *
 * Perche' esiste: il popover era mostrato SEMPRE, anche a header mezzo vuoto,
 * perche' nessuno misurava (l'utente: "hamburger deve apparire solo quando i
 * campi non entrano nell'header"). La versione storica teneva invece la riga
 * SEMPRE, e a colonna stretta il gruppo sessioni collassava a larghezza 0. Le due
 * cadute sono opposte; la cura e' la stessa: MISURARE.
 *
 * Come decide (regola O, misura non stima): la riga (ChatHeadInline) e' montata
 * dentro un host che occupa lo spazio disponibile. `measure()` confronta la
 * larghezza NATURALE della riga (row.scrollWidth, non vincolata perche' i suoi
 * figli non cedono) con lo spazio dell'host (host.clientWidth). La REGOLA di
 * confronto vive nel punto unico chatHeadFitsInline (panel-sizing-logic, regola
 * L); qui c'e' solo la misura e il montaggio.
 *
 * Quando misurare, senza dipendere da ResizeObserver: `measure()` gira in un
 * useLayoutEffect a OGNI render. L'host cambia larghezza per il resize del
 * viewport o il trascinamento del divisore, ed entrambi passano da uno stato di
 * ide-shell (viewportWidth, rightWidth): ide-shell ri-renderizza, questo figlio
 * con lui, e il layout effect rimisura sul DOM gia' aggiornato. Coperto anche il
 * caricamento lazy del ProfileSelector, che allarga la riga con un altro render.
 * Un ResizeObserver e' aggiunto come rete per il caso raro (resize CSS senza
 * render), ma NON e' il meccanismo primario: in alcuni ambienti headless non
 * consegna le callback, e la testata non deve dipenderne.
 *
 * Quando non ci sta, la riga resta nel DOM ma fuori dal flusso (assoluta,
 * invisibile): continua a esporre la larghezza naturale, cosi' se la colonna si
 * riallarga si torna in riga. Un solo montaggio dei controlli; il popover trigger
 * si aggiunge sopra.
 */
export function ChatHead(props: ChatHeadPopoverProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const rowRef = useRef<HTMLDivElement>(null);
  const [inline, setInline] = useState(true);

  const measure = useCallback(() => {
    const host = hostRef.current;
    const row = rowRef.current;
    if (!host || !row) return;
    setInline((current) => chatHeadFitsInline(host.clientWidth, row.scrollWidth, current));
  }, []);

  // Ad ogni render: cattura i cambi di larghezza dell'host (viewport, divisore) e
  // della riga (contenuto, load del selector) senza dipendere da ResizeObserver.
  // setInline fa bail-out se il verdetto non cambia, quindi non innesca loop.
  useLayoutEffect(() => {
    measure();
  });

  // Rete di sicurezza: resize della finestra (l'evento arriva anche quando, per
  // qualche motivo, questo componente non si e' ri-renderizzato) e ResizeObserver
  // per i resize puramente CSS. Nessuno dei due e' garantito ovunque; il layout
  // effect sopra e' il meccanismo su cui si conta.
  useEffect(() => {
    const host = hostRef.current;
    const row = rowRef.current;
    window.addEventListener("resize", measure);
    const observer = new ResizeObserver(measure);
    if (host) observer.observe(host);
    if (row) observer.observe(row);
    return () => {
      window.removeEventListener("resize", measure);
      observer.disconnect();
    };
  }, [measure]);

  return (
    <div
      ref={hostRef}
      style={{
        flex: 1,
        minWidth: 0,
        display: "flex",
        alignItems: "center",
        overflow: "hidden",
        position: "relative",
      }}
    >
      <div
        ref={rowRef}
        aria-hidden={!inline}
        style={
          inline
            ? { display: "flex", alignItems: "center" }
            : {
                // Fuori dal flusso e invisibile, ma ancora misurabile a larghezza
                // naturale: e' la sonda che permette di tornare in riga.
                position: "absolute",
                left: 0,
                top: 0,
                visibility: "hidden",
                pointerEvents: "none",
              }
        }
      >
        <ChatHeadInline {...props} />
      </div>
      {!inline && <ChatHeadPopover {...props} />}
    </div>
  );
}
