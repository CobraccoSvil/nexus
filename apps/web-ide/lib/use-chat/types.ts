// Tipi e costanti condivisi dal hook useChat e dai suoi helper.
// Estratti da use-chat.ts (refactor god-file) senza alcun cambiamento di comportamento.

export const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export type BusyAction = "resend" | "delete" | "feedback" | "feedback-positive";

/** Entry semantica della timeline meta-step (plan/routing/clarify/fallback/reflection). */
export interface MetaStepEntry {
  kind: string;
  title: string;
  payload: Record<string, unknown>;
  correlationId?: string | null;
  createdAt: string;
}
