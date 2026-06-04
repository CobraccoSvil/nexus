"use client";

import { useState } from "react";
import { useEventOfKind } from "../../lib/project-dispatcher/hooks";
import { ProviderBadge } from "./provider-badge";

/**
 * Card collassabile per visualizzare i meta-step semantici pubblicati dal
 * backend durante un run agente:
 *  - plan       → piano del planner_node (lista todo)
 *  - routing    → intent classificato + profilo + behavior_mode
 *  - clarify    → richiesta di chiarimento (Fase 2)
 *  - fallback   → cambio automatico di provider/modello su errore o cooldown
 *  - reflection → riassunto post-hoc del turno
 *
 * Tutti collassati di default tranne `clarify` (richiede risposta utente).
 */

type MetaStepKind = "plan" | "routing" | "clarify" | "fallback" | "reflection" | string;

export interface AgentMetaStepData {
  kind: MetaStepKind;
  title: string;
  payload: Record<string, unknown>;
  correlationId?: string | null;
  createdAt: string;
}

interface KindDescriptor {
  icon: string;
  label: string;
  tone: string; // tailwind text color
  bg: string;   // tailwind bg color
  defaultOpen: boolean;
}

const KIND_MAP: Record<string, KindDescriptor> = {
  plan: {
    icon: "□",
    label: "Piano",
    tone: "text-indigo-700 dark:text-indigo-300",
    bg: "bg-indigo-50 dark:bg-indigo-950/40 border-indigo-200 dark:border-indigo-800",
    defaultOpen: false,
  },
  routing: {
    icon: "→",
    label: "Routing",
    tone: "text-slate-600 dark:text-slate-300",
    bg: "bg-slate-50 dark:bg-slate-900/40 border-slate-200 dark:border-slate-800",
    defaultOpen: false,
  },
  clarify: {
    icon: "?",
    label: "Chiarimento",
    tone: "text-amber-700 dark:text-amber-300",
    bg: "bg-amber-50 dark:bg-amber-950/40 border-amber-300 dark:border-amber-800",
    defaultOpen: true,
  },
  fallback: {
    icon: "↻",
    label: "Fallback",
    tone: "text-orange-700 dark:text-orange-300",
    bg: "bg-orange-50 dark:bg-orange-950/40 border-orange-200 dark:border-orange-800",
    defaultOpen: false,
  },
  reflection: {
    icon: "◐",
    label: "Riflessione",
    tone: "text-purple-700 dark:text-purple-300",
    bg: "bg-purple-50 dark:bg-purple-950/40 border-purple-200 dark:border-purple-800",
    defaultOpen: false,
  },
  // Live UX: ogni tool eseguito dall'executor emette questo meta_step
  // (vedi brain/agents/nodes.py tool_dispatch_node). Card compatta, collassata:
  // l'utente vede il flusso dei tool in tempo reale durante run lunghi.
  tool_executed: {
    // Icona distinta dal chevron di espansione (anch'esso "▸"): altrimenti la
    // card tool mostrava due frecce identiche "▸▸".
    icon: "◆",
    label: "Tool",
    tone: "text-emerald-700 dark:text-emerald-300",
    bg: "bg-emerald-50 dark:bg-emerald-950/40 border-emerald-200 dark:border-emerald-800",
    defaultOpen: false,
  },
};

const DEFAULT_DESC: KindDescriptor = {
  icon: "•",
  label: "Step",
  tone: "text-slate-600 dark:text-slate-300",
  bg: "bg-slate-50 dark:bg-slate-900/40 border-slate-200 dark:border-slate-800",
  defaultOpen: false,
};

// M15.1 — Checklist todo del piano con aggiornamento LIVE via eventi TodoUpdated.
// Lo stato iniziale viene dal payload del meta_step plan; gli aggiornamenti
// arrivano in tempo reale (la checklist si spunta mentre l'agente lavora).
type PlanTodo = { id?: string; seq?: number; content?: string; status?: string; priority?: string };
function PlanChecklist({ todos }: { todos: PlanTodo[] }) {
  // overrides[todo_id] = status piu' recente ricevuto via SSE.
  const [overrides, setOverrides] = useState<Record<string, string>>({});
  useEventOfKind(
    "TodoUpdated",
    (env) => {
      const p = env.payload as { todo_id: string; status: string };
      setOverrides((prev) => (prev[p.todo_id] === p.status ? prev : { ...prev, [p.todo_id]: p.status }));
    },
    [],
  );
  if (!todos.length) return <em className="text-xs opacity-70">Nessun todo</em>;
  const MARK: Record<string, string> = {
    completed: "[x]", in_progress: "[~]", blocked: "[!]", skipped: "[-]", pending: "[ ]",
  };
  return (
    <ol className="list-none pl-0 space-y-0.5 text-xs">
      {todos.map((t, i) => {
        const status = (t.id ? overrides[t.id] : undefined) ?? t.status ?? "pending";
        return (
          <li key={t.id ?? i} className="leading-snug flex items-start gap-1.5">
            <span className="font-mono opacity-70">{MARK[status] ?? MARK.pending}</span>
            <span>{t.content ?? "—"}{t.priority && t.priority !== "normal" ? <span className="opacity-60"> ({t.priority})</span> : null}</span>
          </li>
        );
      })}
    </ol>
  );
}

function renderPayload(kind: string, payload: Record<string, unknown>) {
  if (kind === "plan") {
    const todos = (payload.todos ?? []) as PlanTodo[];
    return <PlanChecklist todos={todos} />;
  }
  if (kind === "routing") {
    const intent = String(payload.intent ?? "—");
    const profile = payload.profile_name as string | undefined;
    const mode = payload.behavior_mode as string | undefined;
    const budget = payload.token_budget as number | undefined;
    return (
      <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-0.5 text-xs">
        <dt className="opacity-70">Intent</dt><dd>{intent}</dd>
        {profile && (<><dt className="opacity-70">Profilo</dt><dd>{profile}</dd></>)}
        {mode && (<><dt className="opacity-70">Modalità</dt><dd>{mode}</dd></>)}
        {typeof budget === "number" && (<><dt className="opacity-70">Token budget</dt><dd>{budget}</dd></>)}
      </dl>
    );
  }
  if (kind === "fallback") {
    return (
      <dl className="grid grid-cols-[auto_1fr] gap-x-2 gap-y-0.5 text-xs">
        <dt className="opacity-70">A</dt><dd>{String(payload.to_provider ?? "?")}/{String(payload.to_model ?? "?")}</dd>
        <dt className="opacity-70">Motivo</dt><dd>{String(payload.reason ?? "—")}</dd>
        <dt className="opacity-70">Tentativo</dt><dd>#{String(payload.attempt ?? "?")}</dd>
      </dl>
    );
  }
  if (kind === "clarify") {
    const question = String(payload.question ?? "");
    const rationale = payload.rationale as string | undefined;
    return (
      <div className="space-y-1 text-xs">
        <p className="font-medium">{question}</p>
        {rationale && <p className="opacity-70 italic">{rationale}</p>}
      </div>
    );
  }
  if (kind === "reflection") {
    const summary = String(payload.summary ?? "");
    return <p className="text-xs leading-snug">{summary}</p>;
  }
  if (kind === "tool_executed") {
    const tool = String(payload.tool ?? "");
    const target = String(payload.target ?? "");
    const isErr = Boolean(payload.is_error);
    return (
      <div className="text-xs flex items-center gap-2 leading-snug">
        <code className={isErr ? "text-red-700 dark:text-red-300" : ""}>{tool}</code>
        {target && <span className="opacity-70 truncate">{target}</span>}
        {isErr && <span className="text-red-600 dark:text-red-300">errore</span>}
      </div>
    );
  }
  // fallback: JSON
  return (
    <pre className="text-[10px] overflow-x-auto opacity-80">
      {JSON.stringify(payload, null, 2)}
    </pre>
  );
}

export function AgentMetaStepCard({ data }: { data: AgentMetaStepData }) {
  const desc = KIND_MAP[data.kind] ?? DEFAULT_DESC;
  const [open, setOpen] = useState(desc.defaultOpen);

  // I meta_step tool arrivano col title gia' prefissato "tool <nome>": col label
  // "Tool" del descrittore diventerebbe "Tool tool <nome>" ("Tooltool"). Rimuovo
  // il prefisso ridondante per i soli tool_executed.
  const displayTitle =
    data.kind === "tool_executed" ? data.title.replace(/^tool\s+/i, "") : data.title;

  // Provider/model per il turno (per il badge colorato per provider+costo).
  // - routing      -> popolato direttamente nel payload (provider/model)
  // - tool_executed -> idem
  // - fallback     -> usa to_provider/to_model (destinazione del fallback)
  // Per gli altri kind (plan, clarify, reflection) il badge non e' mostrato:
  // non hanno semantica di "scelta del modello per il turno".
  const provider = (data.payload?.provider
    ?? data.payload?.to_provider
    ?? null) as string | null;
  const model = (data.payload?.model
    ?? data.payload?.to_model
    ?? null) as string | null;
  const showBadge =
    (provider || model) &&
    (data.kind === "routing" ||
      data.kind === "tool_executed" ||
      data.kind === "fallback");

  return (
    <div
      data-meta-step-kind={data.kind}
      className={`my-1 rounded border ${desc.bg} text-sm`}
    >
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className={`w-full flex items-center gap-2 px-2 py-1 ${desc.tone} hover:opacity-90`}
        aria-expanded={open}
      >
        <span aria-hidden className="font-mono">{open ? "▾" : "▸"}</span>
        <span aria-hidden className="font-mono">{desc.icon}</span>
        <span className="font-medium">{desc.label}</span>
        <span className="opacity-70 truncate text-left flex-1">{displayTitle}</span>
        {showBadge && <ProviderBadge provider={provider} model={model} />}
      </button>
      {open && (
        <div className="px-3 pb-2">
          {renderPayload(data.kind, data.payload)}
        </div>
      )}
    </div>
  );
}
