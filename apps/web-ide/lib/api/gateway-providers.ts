/**
 * IL TIPO DEL WIRE dei provider del gateway, dichiarato UNA VOLTA SOLA.
 *
 * Prima viveva in due posti indipendenti e senza alcun legame: `GwEntry` in
 * `components/ide-shell.tsx` (CON i campi di prontezza) e `GatewayProvider` in
 * `components/settings/provider-settings.tsx` (SENZA). Il secondo rendeva i
 * suoi consumatori ciechi ai campi nuovi per COSTRUZIONE DEL TIPO: non c'era
 * niente da dimenticare di leggere, il campo semplicemente non esisteva.
 *
 * MISURATO a schermo il 10/08/2026 su /admin: `vllm` arriva dal wire con
 * `readiness: "not_configured"` — «nessuno ha chiesto che funzionasse» — e la
 * pagina scriveva «mai misurato», che e' il significato di un'ALTRA variante.
 * Il ramo `healthy === null` collassava tre casi con rimedi opposti:
 * non configurato (niente da fare), in attesa della prima misura (aspettare),
 * stallo (serve un intervento). E' la regola Q vista dal lato del consumatore:
 * il produttore aveva smesso di usare un booleano a tre stati, il consumatore
 * no.
 *
 * E' la stessa forma del difetto «costi a $0.00 per camelCase», chiuso con la
 * fixture di confine condivisa `__wire__/session-usage.json`: un solo tipo, e
 * una fixture che i due lati leggono.
 */

/** Le varianti di prontezza dichiarate da `mcp-core::provider_readiness`. */
export type ProviderReadiness =
  | "not_configured"
  | "awaiting_first_probe"
  | "stalled"
  | "healthy"
  | "down";

export type ReadinessCycle = "periodic_probe" | "reprobe";
export type ReadinessCause = "no_models" | "no_verification_cycle";

/**
 * Le varianti di copertura dichiarate da `mcp-core::provider_declaration`.
 *
 * ORTOGONALE alla prontezza, e i due campi non si possono fondere: un fornitore
 * puo' essere `healthy` e non avere una sola riga di capability — e' il caso
 * reale di groq e openrouter, misurato il 10/08/2026. Il pannello li mostra su
 * due righe perche' rispondono a due domande con due rimedi diversi.
 */
export type ProviderDeclaration =
  | "nothing_to_declare"
  | "complete"
  | "partial"
  | "absent";

/** Una entry di `GET /api/gateway/providers`. */
export interface GatewayProvider {
  name: string;
  /**
   * `null` NON significa «mai misurato»: significa che non c'e' una misura, e
   * il PERCHE' lo dice `readiness`. Leggere questo campo da solo e' il difetto.
   */
  healthy: boolean | null;
  configured?: boolean;
  last_check?: string;
  last_health_check_at?: string;
  error?: string;
  cooldown_seconds_remaining?: number;
  readiness?: ProviderReadiness;
  readiness_cycle?: ReadinessCycle;
  readiness_cause?: ReadinessCause;
  readiness_models?: number;
  declaration?: ProviderDeclaration;
  /** Quanti modelli ABILITATI sono privi di capability. Assente dove non ne manca nessuno. */
  declaration_undeclared?: number;
}

/**
 * Cio' che un pannello mostra per una entry: l'ETICHETTA breve e se il caso
 * RICHIEDE UN INTERVENTO.
 *
 * Il secondo campo non e' un dettaglio di stile: e' il solo modo perche' uno
 * stallo — l'unica variante che nessun ciclo risolvera' da solo — si distingua
 * da un'attesa, che invece si risolve aspettando.
 */
export interface RenderedReadiness {
  label: string;
  requiresAction: boolean;
}

/**
 * L'etichetta breve per una entry, DERIVATA dai campi (regola Q punto 3).
 *
 * Il cooldown ha la precedenza che aveva gia' in `ide-shell`: e' scritto da
 * mcp-core indipendentemente da `readiness`, e dice una cosa che l'utente deve
 * sapere prima di tutto il resto — quel provider e' escluso adesso, e per
 * quanto.
 *
 * L'IGNOTO non degrada: una entry senza `readiness` viene da un backend che non
 * parla questa versione del contratto, e lo dichiara invece di fingere una
 * misura mai fatta.
 */
export function renderReadiness(p: GatewayProvider): RenderedReadiness {
  const cooldown = p.cooldown_seconds_remaining;
  if (typeof cooldown === "number" && cooldown > 0) {
    // Causa E pausa, non l'una AL POSTO dell'altra. Osservato il 10/08/2026:
    // con la sola pausa, i tre provider fuori per credito perdevano
    // `credit_balance_too_low`, che e' l'unica delle due informazioni su cui un
    // amministratore puo' AGIRE — la pausa, da sola, si ripresenta uguale al
    // termine se il credito non e' stato ricaricato.
    const pausa = `in pausa per ${Math.ceil(cooldown / 60)} min`;
    const causa = p.error?.split(":")[0]?.trim();
    return {
      label: causa ? `${causa}, ${pausa}` : pausa,
      requiresAction: false,
    };
  }
  switch (p.readiness) {
    case "healthy":
      return { label: "attivo", requiresAction: false };
    case "down":
      return { label: p.error?.split(":")[0] ?? "errore", requiresAction: false };
    case "not_configured":
      return { label: "non configurato", requiresAction: false };
    case "awaiting_first_probe":
      return { label: "in attesa della prima verifica", requiresAction: false };
    case "stalled":
      return { label: "fermo: serve un intervento", requiresAction: true };
    default:
      // Campo assente: il backend non dichiara la prontezza. Si ripiega sul
      // solo fatto disponibile, e non si inventa una causa.
      if (p.healthy === true) return { label: "attivo", requiresAction: false };
      if (p.healthy === false) {
        return { label: p.error?.split(":")[0] ?? "errore", requiresAction: false };
      }
      return { label: "prontezza non dichiarata", requiresAction: false };
  }
}

/**
 * L'etichetta della COPERTURA DELLA DICHIARAZIONE, o `null` quando non c'e'
 * nulla da dire.
 *
 * `null` non e' un esito conflazionato (regola Q): l'esito sta nel campo
 * `declaration`, e questo e' solo il verdetto su cosa MOSTRARE. Stampare
 * «dichiarazione completa» accanto a ogni fornitore in ordine sarebbe rumore, e
 * il rumore e' il modo in cui una riga che conta smette di essere letta.
 *
 * Le due mancanze restano DUE frasi perche' hanno due rimedi: `absent` e' il
 * fornitore onboardato senza la sua migrazione di capability (un atto solo),
 * `partial` e' il catalogo vivo che ha aggiunto modelli dopo quella migrazione
 * (un rimedio per-modello, ricorrente).
 *
 * L'IGNOTO non diventa un allarme: una entry senza `declaration` viene da un
 * backend che non parla questa versione del contratto, e di lui non sappiamo se
 * manchi qualcosa.
 */
export function renderDeclaration(p: GatewayProvider): RenderedReadiness | null {
  const mancanti = p.declaration_undeclared;
  switch (p.declaration) {
    case "absent":
      return {
        label:
          mancanti === undefined
            ? "nessuna capability dichiarata"
            : `nessuna capability dichiarata (${mancanti} modelli)`,
        requiresAction: true,
      };
    case "partial":
      return {
        label:
          mancanti === undefined
            ? "capability incomplete"
            : `${mancanti} modelli senza capability`,
        requiresAction: true,
      };
    default:
      // `complete`, `nothing_to_declare`, o campo assente: niente da mostrare.
      return null;
  }
}
