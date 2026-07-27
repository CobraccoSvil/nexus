// La scelta di provider del composer: PREFERENZA o PIN.
//
// Logica pura estratta dal JSX per essere testabile senza React — stesso pattern
// di interrupt-button-logic.ts e usage-badge-logic.ts.
//
// PERCHE' ESISTE. Nel composer ci sono due controlli: il dropdown del provider e
// il pulsante "Forza". Il pulsante non e' mai arrivato al backend — serviva solo
// al colore del bordo e a mostrare il dropdown dei modelli — quindi il backend
// riceveva UNA sola informazione, il nome del provider, e doveva dedurre quanto
// vincolasse. Finche' l'override non aveva effetto reale la deduzione era
// innocua (rispondeva un altro provider: era il difetto stesso). Da quando il
// provider scelto viaggia come pin (gateway in strict, chain di un solo
// fornitore, nessun fallback cross-provider), la stessa deduzione
// trasformerebbe OGNI selezione dal dropdown in un vincolo duro — e i due
// tooltip del pulsante, che promettono l'opposto, direbbero il falso.
//
// Quindi: il pin duro scatta SOLO col pulsante attivo, viaggia dichiarato sul
// wire (`providerOverrideMode`, identificatori canonici inglesi come
// automationMode/supervisorMode) e i tooltip nascono qui, accanto alla
// condizione che li rende veri.

/** Valore del dropdown provider quando l'utente non ne sceglie nessuno. */
export const PROVIDER_AUTO = "auto";

/**
 * Quanto vincola il provider scelto. Identificatori canonici (inglese, un solo
 * nome per stato): sono gli stessi che il backend parsa in
 * `ProviderOverrideMode::try_parse`.
 */
export type ProviderOverrideMode = "preferred" | "pinned";

export interface ProviderChoiceInput {
  /** Valore del dropdown: `PROVIDER_AUTO` oppure il nome del provider. */
  selectedProvider: string;
  /** Stato del pulsante "Forza". */
  forceProvider: boolean;
  /**
   * Provider suggerito da una superficie esterna (es. generazione documenti,
   * che chiede un modello capace). Vale solo quando il dropdown e' su "Auto".
   */
  hintProvider?: string;
}

export interface ProviderChoiceWire {
  /** `providerOverride` del corpo della POST. Assente = decide il routing. */
  providerOverride?: string;
  /** `providerOverrideMode` del corpo della POST. Sempre dichiarato. */
  providerOverrideMode: ProviderOverrideMode;
}

/**
 * Il pin e' duro solo per una scelta ESPLICITA dal dropdown col pulsante
 * attivo.
 *
 * La congiunzione non e' ridondante: `forceProvider` e' uno stato locale che
 * sopravvive al ritorno del dropdown su "Auto" (il pulsante si limita a
 * sparire dalla barra, nessuno lo rimette a false). Senza il primo termine, un
 * invio guidato da un hint esterno erediterebbe un "Forza" premuto dieci minuti
 * prima per un altro provider — un vincolo che l'utente non vede piu' e non ha
 * chiesto per questa richiesta.
 */
export function isProviderPinned(selectedProvider: string, forceProvider: boolean): boolean {
  return selectedProvider !== PROVIDER_AUTO && forceProvider;
}

/**
 * Cosa viaggia nel corpo della POST della chat.
 *
 * Il provider dell'hint esterno resta sempre una PREFERENZA: e' una scelta del
 * sistema (serve un modello capace), non un ordine dell'utente, e togliere il
 * fallback a una richiesta che l'utente non ha vincolato la renderebbe solo piu'
 * fragile.
 */
export function providerChoiceForSend(input: ProviderChoiceInput): ProviderChoiceWire {
  const { selectedProvider, forceProvider, hintProvider } = input;
  const chosen = selectedProvider !== PROVIDER_AUTO ? selectedProvider : hintProvider;
  return {
    providerOverride: chosen,
    providerOverrideMode: isProviderPinned(selectedProvider, forceProvider)
      ? "pinned"
      : "preferred",
  };
}

/**
 * DOVE ARRIVA IL PIN, OGGI. Il vincolo duro viaggia fino al gateway solo sul
 * turno singolo, cioe' in modalita' `study`. In `confirm` e `automatic`
 * l'handler devia su `spawn_agent_run` e passa il solo NOME del provider:
 * `SpawnAgentParams` non ha un campo che trasporti la FORZA del vincolo, quindi
 * il pin muore al confine dell'handler e da li' in poi il provider e' solo il
 * punto di partenza, con il failover cross-provider dell'esecutore attivo.
 *
 * Finche' e' cosi', queste funzioni devono dirlo: un tooltip che promette "va
 * solo a X" nella modalita' di default (`confirm`) ripeterebbe in una frase
 * nuova esattamente il difetto che il pin e' nato per chiudere — la UI che
 * dichiara cio' che il backend non fa.
 */
export type AutomationMode = "study" | "confirm" | "automatic";

/** `true` se in questa modalita' il pin duro arriva davvero al gateway. */
export function pinArrivaAlGateway(automationMode: AutomationMode): boolean {
  return automationMode === "study";
}

/** Tooltip del dropdown provider: dice cosa succede DAVVERO in ogni stato. */
export function providerSelectTitle(
  selectedProvider: string,
  forceProvider: boolean,
  automationMode: AutomationMode,
): string {
  if (selectedProvider === PROVIDER_AUTO) {
    return "Routing automatico: sceglie il modello migliore per ogni task";
  }
  if (isProviderPinned(selectedProvider, forceProvider)) {
    if (!pinArrivaAlGateway(automationMode)) {
      return (
        `Provider ${selectedProvider}: il pin vale solo in modalita' Studio. ` +
        `Qui e' il punto di partenza, e se non risponde il run passa a un altro ` +
        `fornitore.`
      );
    }
    return (
      `Provider ${selectedProvider} PINNATO: la richiesta va solo a lui, ` +
      `nessun ripiego su un altro provider. Disattiva "Forza" per lasciare il ` +
      `fallback, o torna ad Auto per il routing intelligente.`
    );
  }
  return (
    `Preferenza ${selectedProvider}: il routing parte da qui ma puo' scegliere ` +
    `un altro provider (fallback attivo). Attiva "Forza" per vincolare la ` +
    `richiesta a ${selectedProvider}` +
    (pinArrivaAlGateway(automationMode) ? "." : " nella modalita' Studio.")
  );
}

export interface ForceButtonView {
  label: string;
  title: string;
}

/**
 * Etichetta e tooltip del pulsante "Forza".
 *
 * I due tooltip precedenti — "Override attivo: il provider selezionato viene
 * forzato" e "Override disattivo: il routing puo' scegliere un provider
 * diverso" — descrivevano una differenza che il wire non portava: acceso o
 * spento, partiva la stessa richiesta. Erano accidentalmente veri finche'
 * l'override non aveva effetto, e sarebbero diventati falsi appena il pin ha
 * cominciato a funzionare. Ora la differenza esiste, e il tooltip nomina la
 * conseguenza (il fallback c'e' o non c'e') invece di ripetere il nome del
 * pulsante.
 */
export function forceButtonView(
  selectedProvider: string,
  forceProvider: boolean,
  automationMode: AutomationMode,
): ForceButtonView {
  if (isProviderPinned(selectedProvider, forceProvider)) {
    if (!pinArrivaAlGateway(automationMode)) {
      // Il pulsante resta premuto (lo stato e' dell'utente, non nostro) ma il
      // segno di spunta no: promettere "va solo a X" qui sarebbe falso, ed e'
      // la stessa bugia da cui il pin e' nato.
      return {
        label: "Forza",
        title:
          `Il pin vale solo in modalita' Studio. In questa modalita' ` +
          `${selectedProvider} e' il punto di partenza del run, che puo' ` +
          `passare a un altro fornitore se non risponde.`,
      };
    }
    return {
      label: "Forza ✓",
      title:
        `Pin attivo: la richiesta va solo a ${selectedProvider}. Se non risponde ` +
        `la chat lo dice, invece di cambiare fornitore in silenzio.`,
    };
  }
  return {
    label: "Forza",
    title:
      `Pin disattivo: ${selectedProvider} e' una preferenza, il routing puo' ` +
      `scegliere un altro provider se serve (fallback attivo)` +
      (pinArrivaAlGateway(automationMode) ? "." : "; il pin vale solo in Studio."),
  };
}
