/**
 * Punto unico (regola L) del calcolo "riempimento context window" mostrato
 * dalla TokenUsageBar ("N% ctx") e dal badge sul bottone "Compatta chat".
 * Prima era duplicato in due punti di chat-panel.tsx (useEffect onCtxRatioChange
 * + IIFE della TokenUsageBar) con lo stesso bug: come numeratore usava
 * `promptTokens`, che nel path agentico e' il CUMULATIVO di tutte le iterazioni
 * del run (billing) — su run multi-iterazione produceva percentuali assurde
 * (es. "5046% ctx" dopo una compattazione, quando l'unico assistant vivo e'
 * proprio il messaggio finale del run gigante appena concluso).
 *
 * Il numeratore corretto e' il prompt dell'ULTIMA chiamata LLM
 * (`lastPromptTokens`), propagato end-to-end dal backend (metadata del
 * messaggio assistant) e dagli eventi live `agent_usage`. Niente fallback al
 * cumulativo: se il campo manca (messaggi storici) la percentuale si nasconde.
 */

import type { AgentRunInfo, ChatMessage, ModelCatalogEntry } from "./api-client";
import { fallbackContextWindow } from "./model-catalog";

export interface ContextFill {
  /** Modello attivo risolto (run in corso > ultimo assistant > selezione manuale). */
  activeModel: string | null;
  /** Context window in token del modello attivo; null se non risolvibile. */
  ctxWindow: number | null;
  /** Prompt token dell'ultima chiamata LLM; null se non disponibile. */
  lastInputTokens: number | null;
  /** lastInputTokens / ctxWindow; puo' superare 1.0 (overflow reale). Null se
      uno dei due input manca: la UI nasconde la percentuale. */
  ratio: number | null;
}

export function computeContextFill(
  messages: ChatMessage[],
  agentRun: AgentRunInfo | null,
  selectedModel: string,
  modelCatalog: ModelCatalogEntry[],
): ContextFill {
  // Ultimo assistant ATTIVO con il riempimento contesto persistito. Esclude i
  // soft-deleted: dopo un compact i vecchi assistant restano nel DB con
  // deletedAt valorizzato e il ratio resterebbe bloccato sul pre-compact.
  const lastAssistantWithCtx = [...messages]
    .reverse()
    .find(
      (m) =>
        m.role === "assistant" &&
        !m.deletedAt &&
        (m.lastPromptTokens ?? 0) > 0,
    );
  const activeModel =
    agentRun?.model ||
    lastAssistantWithCtx?.model ||
    (selectedModel !== "auto" ? selectedModel : null);
  const catalogEntry = activeModel
    ? modelCatalog.find((m) => m.model === activeModel)
    : null;
  // `?? fallback` non basta: un contextWindow 0 dal catalog (colonna NULL lato
  // DB serializzata a 0) deve comunque ricadere sulla stima locale.
  const ctxWindow =
    catalogEntry?.contextWindow && catalogEntry.contextWindow > 0
      ? catalogEntry.contextWindow
      : fallbackContextWindow(activeModel);
  // Live (eventi agent_usage, per-turno) > messaggio persistito. MAI
  // usage.totalPromptTokens: dal DB e' il cumulativo di billing del run.
  const lastInputTokens =
    agentRun?.usage?.lastPromptTokens ??
    lastAssistantWithCtx?.lastPromptTokens ??
    null;
  const ratio =
    ctxWindow && ctxWindow > 0 && lastInputTokens && lastInputTokens > 0
      ? lastInputTokens / ctxWindow
      : null;
  return { activeModel, ctxWindow, lastInputTokens, ratio };
}
