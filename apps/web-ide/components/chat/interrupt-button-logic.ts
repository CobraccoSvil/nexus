// Logica pura del pulsante di interruzione nella barra attivita' del run.
// Estratta dal JSX per essere testabile senza React — stesso pattern di
// usage-badge-logic.ts e step-detail-logic.ts.
//
// PERCHE' ESISTE (regola M: lo stato tecnico si dichiara, non si suggerisce).
// Il pulsante si chiamava "Forza stop" ed era arancione accanto al "Stop" rosso
// del composer. Due difetti misurati, entrambi di etichetta:
//
//   1. NESSUNA ESCALATION ESISTE. I due pulsanti invocano la stessa `cancelRun`
//      e quindi la stessa e unica rotta di cancellazione del backend
//      (`POST /api/chat/agent-runs/:id/cancel` -> `supersede_active_runs`, in
//      cascade sulla sessione). Non c'e' un percorso "piu' forte" da chiamare:
//      il nome prometteva una via che il backend non espone. Chi aveva gia'
//      premuto Stop senza vedere effetto concludeva che serviva il pulsante
//      "forte" e ASPETTAVA invece di indagare.
//   2. LA CONDIZIONE DI COMPARSA NON E' L'INATTIVITA'. Il numero su cui il
//      pulsante compare e' il tempo dall'AVVIO del run (`agent_runs.created_at`),
//      non dall'ultimo passo: la variabile a monte si chiamava
//      `secondsSinceLastStep` ma quel calcolo era stato spostato sull'avvio run
//      (chat-panel) senza rinominare nulla. Un run che lavora attivamente da 3
//      minuti mostra quindi il pulsante esattamente come un run bloccato. Le due
//      grandezze restano DISTINTE qui e il tooltip le dichiara entrambe.

/**
 * Soglia (secondi dall'avvio del run) oltre la quale la barra passa in evidenza
 * arancione e offre l'interruzione. Punto unico: la barra la usa per l'etichetta
 * di stato, per il colore del timer e per la comparsa del pulsante.
 */
export const RUN_LONG_THRESHOLD_SECONDS = 120;

export interface InterruptButtonInput {
  /** Secondi dall'avvio del run (`agent_runs.createdAt`). NON e' inattivita'. */
  runElapsedSeconds: number;
  /** Secondi dall'ultimo step o meta-step ricevuto: questa e' l'inattivita'. */
  secondsSinceLastStep: number;
  /**
   * Segnale di attesa gia' calcolato a monte (chat-panel, soglia unica sui
   * secondi dall'ultimo passo). Si riceve invece di ricalcolarlo per non avere
   * due soglie di "fermo" divergenti.
   */
  isAgentStuck: boolean;
}

export interface InterruptButtonView {
  /** Il pulsante va reso (la barra aggiunge la condizione "esiste un runId"). */
  visible: boolean;
  /** Etichetta del pulsante. */
  label: string;
  /** Tooltip: cosa fa davvero e perche' compare adesso. */
  title: string;
}

export interface ActivityStatusInput {
  /** Secondi dall'avvio del run. */
  runElapsedSeconds: number;
  /** Secondi dall'ultimo step o meta-step: l'inattivita'. */
  secondsSinceLastStep: number;
  /** Attesa reale rilevata a monte (soglia unica sui secondi dall'ultimo passo). */
  isAgentStuck: boolean;
  /** Etichetta ordinaria quando non c'e' niente da segnalare. */
  busyLabel: string;
}

export interface ActivityStatusView {
  /** Testo dell'etichetta di stato. */
  text: string;
  /** Tooltip: entrambi i tempi, sempre, con il loro significato. */
  title: string;
  /** Lo stato merita evidenza arancione (coerente col simbolo di attenzione). */
  warn: boolean;
}

/** "45s" sotto il minuto, "4m 28s" sopra. Usata dal timer e dai tooltip. */
export function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  if (s < 60) return `${s}s`;
  return `${Math.floor(s / 60)}m ${s % 60}s`;
}

/**
 * Etichetta di stato della barra attivita'.
 *
 * PRECEDENZA: l'attesa reale vince sulla durata. Era il contrario — un run oltre
 * i 2 minuti scriveva "AI in elaborazione" anche mentre era fermo da tre, e
 * l'informazione specifica (nessun passo in arrivo) veniva soppressa da quella
 * generica (sta lavorando da tanto). Chi guardava la barra leggeva "elaborazione"
 * e aspettava. Se e' fermo ora lo dice, e dice da quanto: "fermo" senza il tempo
 * non fa decidere nulla.
 *
 * Il tempo scritto nell'etichetta e' l'INATTIVITA'; il timer accanto resta la
 * durata del run. Sono due numeri diversi e il tooltip li nomina entrambi, cosi'
 * chi legge non deve indovinare quale dei due sta guardando.
 */
export function activityStatusView(input: ActivityStatusInput): ActivityStatusView {
  const { runElapsedSeconds, secondsSinceLastStep, isAgentStuck, busyLabel } = input;
  const durata = `Il run dura da ${formatDuration(runElapsedSeconds)}`;
  const inattivita = formatDuration(secondsSinceLastStep);

  if (isAgentStuck) {
    return {
      text: `⚠ Agente in attesa da ${inattivita}`,
      title: `Nessuno step ne' meta-step da ${inattivita}. ${durata}.`,
      warn: true,
    };
  }
  if (runElapsedSeconds > RUN_LONG_THRESHOLD_SECONDS) {
    return {
      text: "⚠ AI in elaborazione",
      title: `${durata}; ultimo passo ${inattivita} fa: l'agente sta lavorando, non e' fermo.`,
      warn: true,
    };
  }
  return {
    text: busyLabel,
    title: `${durata}; ultimo passo ${inattivita} fa.`,
    warn: false,
  };
}

/**
 * Vista del pulsante di interruzione.
 *
 * L'etichetta non promette gradi di forza perche' non ce ne sono: l'azione e'
 * identica a quella del pulsante Stop del composer, e il tooltip lo dice — cosi'
 * chi ha gia' premuto Stop senza effetto sa che ripremere non cambia nulla e che
 * il problema e' altrove.
 *
 * Il tooltip separa sempre i due tempi: quello che ha fatto comparire il
 * pulsante (durata del run) e quello che dice se l'agente sta davvero fermo
 * (inattivita'). Erano confusi in un unico numero, ed e' la ragione per cui la
 * comparsa del pulsante veniva letta come diagnosi di blocco.
 */
export function interruptButtonView(input: InterruptButtonInput): InterruptButtonView {
  const { runElapsedSeconds, secondsSinceLastStep, isAgentStuck } = input;
  const motivo = `il run dura da ${formatDuration(runElapsedSeconds)}`;
  const attivita = isAgentStuck
    ? `nessun passo da ${formatDuration(secondsSinceLastStep)}`
    : `ultimo passo ${formatDuration(secondsSinceLastStep)} fa (l'agente sta ancora lavorando)`;
  return {
    visible: runElapsedSeconds > RUN_LONG_THRESHOLD_SECONDS,
    label: "Interrompi",
    title:
      `Interrompe il run: stessa azione del pulsante Stop, non piu' incisiva. ` +
      `Compare perche' ${motivo}; ${attivita}.`,
  };
}
