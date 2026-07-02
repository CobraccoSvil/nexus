// Costruzione dei riepiloghi testuali di un AgentRun e del messaggio terminale.
// Funzioni pure estratte da use-chat.ts (refactor god-file) senza cambiamenti di comportamento.

import type { AgentRunInfo, ChatMessage } from "../api-client";

export function buildTerminalRunSummary(run: AgentRunInfo): string {
  const completed = run.steps.filter((step) => step.status === "completed").length;
  const failed = run.steps.filter((step) => step.status === "failed").length;
  const awaiting = run.pendingActions.length;

  if (run.status === "completed") {
    return completed > 0
      ? `Operazione completata. Ho eseguito ${completed} step.`
      : "Operazione completata.";
  }
  if (run.status === "failed") {
    if (completed > 0) {
      return `Operazione terminata con errore dopo ${completed} step completati${failed > 0 ? ` e ${failed} falliti` : ""}.`;
    }
    return "Operazione terminata con errore.";
  }
  if (run.status === "timed_out") {
    return completed > 0
      ? `Operazione interrotta per timeout dopo ${completed} step completati.`
      : "Operazione interrotta per timeout prima della risposta finale.";
  }
  if (run.status === "cancelled") {
    return "Operazione annullata.";
  }
  if (run.status === "interrupted") {
    return completed > 0
      ? `Elaborazione interrotta dal riavvio del server dopo ${completed} step. Puoi ripetere la richiesta.`
      : "Elaborazione interrotta dal riavvio del server. Puoi ripetere la richiesta.";
  }
  if (run.status === "loop_aborted") {
    return completed > 0
      ? `Operazione interrotta: il modello era entrato in un ciclo ripetitivo dopo ${completed} step. Al prossimo invio verrà usato automaticamente un modello più capace.`
      : "Operazione interrotta: il modello era entrato in un ciclo ripetitivo. Al prossimo invio verrà usato automaticamente un modello più capace.";
  }
  if (run.status === "provider_unavailable") {
    return "Operazione interrotta: tutti i provider AI configurati sono temporaneamente non disponibili (quota esaurita o rate limit). Riprova tra qualche minuto.";
  }
  if (run.status === "awaiting_confirmation") {
    return awaiting > 0
      ? `In attesa di conferma per ${awaiting} azion${awaiting === 1 ? "e" : "i"}.`
      : "In attesa di conferma per proseguire.";
  }
  // Esiti canonici della macchina a stati di terminazione (mig 0386).
  if (run.status === "completed_verified") {
    return completed > 0
      ? `Operazione completata e verificata. Ho eseguito ${completed} step.`
      : "Operazione completata e verificata.";
  }
  if (run.status === "failed_diagnosed") {
    return completed > 0
      ? `Operazione non completata dopo ${completed} step: ho interrotto e diagnosticato il blocco (esito e prossimo passo nel messaggio).`
      : "Operazione non completata: ho interrotto e diagnosticato il blocco (esito e prossimo passo nel messaggio).";
  }
  if (run.status === "blocked_needs_input") {
    return "In attesa di input esterno per proseguire (es. credenziale, permesso o servizio mancante).";
  }
  return "Operazione conclusa.";
}

/** Costruisce un riepilogo dettagliato delle azioni eseguite dall'agente (P2). */
export function buildSemanticDetail(run: AgentRunInfo): string {
  const WRITE_TOOLS = new Set(["write_file", "edit_file", "create_file", "patch_file"]);
  const CMD_TOOLS = new Set(["run_in_terminal", "run_command"]);
  const READ_TOOLS = new Set(["read_file", "search_in_files", "search_files"]);
  const IGNORE_TOOLS = new Set(["supervisor_check"]);

  const modifiedFiles: string[] = [];
  const commands: string[] = [];
  let analysisCount = 0;
  let errorCount = 0;

  for (const step of run.steps) {
    if (IGNORE_TOOLS.has(step.toolName)) continue;
    if (step.status === "failed") errorCount++;

    if (WRITE_TOOLS.has(step.toolName)) {
      const path = (step.toolInput?.path || step.toolInput?.file_path || step.toolInput?.filename) as string | undefined;
      if (path && !modifiedFiles.includes(path)) {
        modifiedFiles.push(path);
      }
    } else if (CMD_TOOLS.has(step.toolName)) {
      const cmd = (step.toolInput?.command || step.toolInput?.cmd || step.toolInput?.text) as string | undefined;
      if (cmd) {
        // Tronca comandi molto lunghi
        const short = cmd.length > 80 ? cmd.slice(0, 77) + "..." : cmd;
        if (!commands.includes(short)) commands.push(short);
      }
    } else if (READ_TOOLS.has(step.toolName)) {
      analysisCount++;
    }
  }

  // Se non ci sono azioni significative, non generare dettagli
  if (modifiedFiles.length === 0 && commands.length === 0 && analysisCount === 0) {
    return "";
  }

  const lines: string[] = [];
  if (modifiedFiles.length > 0) {
    const MAX_FILES = 5;
    const shown = modifiedFiles.slice(0, MAX_FILES).map((f) => {
      // Mostra solo il nome del file (senza path lungo)
      const parts = f.replace(/\\/g, "/").split("/");
      return `\`${parts.length > 2 ? ".../" + parts.slice(-2).join("/") : f}\``;
    });
    const extra = modifiedFiles.length > MAX_FILES ? ` e altri ${modifiedFiles.length - MAX_FILES} file` : "";
    lines.push(`- Modificati ${modifiedFiles.length} file: ${shown.join(", ")}${extra}`);
  }
  if (commands.length > 0) {
    const MAX_CMDS = 3;
    const shown = commands.slice(0, MAX_CMDS).map((c) => `\`${c}\``);
    const extra = commands.length > MAX_CMDS ? ` e altri ${commands.length - MAX_CMDS}` : "";
    lines.push(`- Eseguiti ${commands.length} comandi: ${shown.join(", ")}${extra}`);
  }
  if (analysisCount > 0) {
    lines.push(`- Analizzati ${analysisCount} file`);
  }

  const completed = run.steps.filter((s) => s.status === "completed").length;
  lines.push(`- Risultato: ${completed} step completati${errorCount > 0 ? `, ${errorCount} errori` : ""}`);

  return `\n\n**Riepilogo:**\n${lines.join("\n")}`;
}

/**
 * Indica se la finalAnswer e' un epitaffio insignificante (placeholder di
 * supersede/annullamento) anziche' una risposta reale del modello. Strutturale,
 * non lessicale sul contenuto utile: confronta solo coi placeholder noti.
 */
function isEmptyOrEpitaph(finalAnswer?: string): boolean {
  const fa = (finalAnswer ?? "").trim();
  return (
    fa.length === 0 ||
    fa === "Superato da un nuovo run." ||
    fa === "Operazione annullata."
  );
}

export function createTerminalMessage(
  run: AgentRunInfo,
  pid: string,
  lastStreamingText?: string,
): ChatMessage | null {
  const statusSummary = buildTerminalRunSummary(run);
  const semanticDetail = buildSemanticDetail(run);

  let baseContent: string;
  if (run.finalAnswer?.trim() && run.finalAnswer.trim().length > 0) {
    // La risposta finale del modello e' presente: usala, appendi il dettaglio semantico
    baseContent = run.finalAnswer + semanticDetail;
  } else if (lastStreamingText?.trim() && lastStreamingText.trim().length > 0) {
    // Testo streaming parziale: usalo, appendi il dettaglio semantico
    baseContent = lastStreamingText + semanticDetail;
  } else {
    // Nessuna risposta dal modello: usa status + dettaglio semantico
    baseContent = statusSummary + semanticDetail;
  }

  // Run CANCELLATO/SUPERATO (es. superato da un nuovo messaggio, last-wins): il
  // suo esito viene pubblicato DOPO il messaggio utente successivo, quindi senza
  // un'intestazione esplicita sembra la risposta alla NUOVA domanda (incidente
  // reale: l'epitaffio del run letto come risposta all'error-fix).
  // Etichetta basata sul FATTO strutturale run.status — mai sul testo.
  if (run.status === "cancelled") {
    if (isEmptyOrEpitaph(run.finalAnswer) && !(lastStreamingText?.trim())) {
      // Run superato/annullato SENZA una risposta vera ne' testo streaming:
      // NON ritornare null (la bolla sparirebbe e l'utente non vedrebbe l'esito,
      // incidente reale tipico col provider primario in cooldown). Costruisci
      // comunque un messaggio terminale minimo e informativo, distinguendo il
      // supersede (placeholder "Superato da un nuovo run.") dall'annullamento
      // manuale, sempre basandosi sul FATTO strutturale.
      const fa = (run.finalAnswer ?? "").trim();
      const minimal =
        fa === "Superato da un nuovo run."
          ? "Operazione interrotta (superata da una nuova richiesta)."
          : "Operazione annullata.";
      // Se nel frattempo il run ha comunque prodotto azioni, accodiamo il
      // riepilogo semantico cosi' l'utente vede cosa e' stato fatto prima dello stop.
      baseContent = `> **Attività precedente interrotta** — è il riepilogo di un lavoro che era in corso e si è interrotto perché nel frattempo è partito un nuovo turno (un tuo nuovo messaggio o un comando in background concluso). **Non è la risposta al tuo ultimo messaggio**.\n\n${minimal}${semanticDetail}`;
    } else {
      // C'e' una risposta reale (il run aveva gia' prodotto un esito prima del
      // supersede): la mostriamo, ma con un'intestazione che chiarisce il contesto.
      baseContent = `> **Attività precedente interrotta** — è il riepilogo di un lavoro che era già in corso e si è interrotto perché nel frattempo è partito un nuovo turno (un tuo nuovo messaggio o un comando in background concluso). **Non è la risposta al tuo ultimo messaggio**: se è rimasto incompleto, richiedilo di nuovo.\n\n${baseContent}`;
    }
  }

  // Difesa finale: per QUALUNQUE run terminato non lasciare mai un contenuto
  // vuoto/insignificante (es. provider AI in cooldown/credito esaurito che
  // chiude senza testo, reasoner che termina senza output). Senza questo,
  // baseContent potrebbe ridursi a una stringa vuota e la bolla sparirebbe.
  // Ricade sul riepilogo di stato (sempre non vuoto), con nota provider se nota.
  if (!baseContent.trim()) {
    const providerHint =
      run.status === "provider_unavailable"
        ? "Elaborazione interrotta: provider AI non disponibile (cooldown/credito)."
        : statusSummary;
    baseContent = providerHint + semanticDetail;
  }

  // Prependi l'avviso privacy se il provider non e' EU/locale
  const content = run.providerPrivacyNotice
    ? `${run.providerPrivacyNotice}\n\n---\n\n${baseContent}`
    : baseContent;

  return {
    id: `agent-${run.runId}`,
    sessionId: run.sessionId,
    projectId: pid,
    role: "assistant",
    content,
    runId: run.runId,
    automationMode: "agent" as const,
    provider: run.provider,
    model: run.model,
    promptTokens: run.usage?.totalPromptTokens,
    // Riempimento contesto dell'ultima iterazione (live): tiene coerente il
    // ratio ctx% del messaggio sintetico con quello persistito dal backend.
    lastPromptTokens: run.usage?.lastPromptTokens,
    completionTokens: run.usage?.totalCompletionTokens,
    totalTokens: run.usage?.totalTokens,
    totalCost: run.totalCostUsd,
    createdAt: run.completedAt ?? new Date().toISOString(),
  };
}
