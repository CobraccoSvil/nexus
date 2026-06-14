"use client";

import { useEffect, useRef, useState } from "react";
import type { AgentStep } from "../../lib/api-client";
import { toolLabel } from "./tool-labels";

/* ------------------------------------------------------------------ */
/* AgentPreparingBubble  (P1)                                          */
/* ------------------------------------------------------------------ */

export function AgentPreparingBubble({ tc }: { tc: Record<string, string> }) {
  const [seconds, setSeconds] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setSeconds((s) => s + 1), 1000);
    return () => clearInterval(id);
  }, []);
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 10,
        padding: "10px 14px",
        borderRadius: 10,
        background: tc.bgCard,
        border: `1px solid ${tc.border}`,
        alignSelf: "flex-start",
        maxWidth: "80%",
      }}
    >
      <span
        style={{
          width: 10,
          height: 10,
          borderRadius: "50%",
          background: "#22c55e",
          animation: "pulse 1.4s ease-in-out infinite",
          flexShrink: 0,
        }}
      />
      <span style={{ color: tc.textMuted, fontSize: 13, fontStyle: "italic" }}>
        Nexus sta preparando l&apos;esecuzione&hellip;
      </span>
      <span style={{ color: tc.textMuted, fontSize: 11, opacity: 0.7 }}>
        {seconds}s
      </span>
    </div>
  );
}

/* ------------------------------------------------------------------ */
/* ThinkingBlock  — ragionamento intermedio del modello                 */
/* ------------------------------------------------------------------ */

export function ThinkingBlock({ text, tc }: { text: string; tc: Record<string, string> }) {
  const [expanded, setExpanded] = useState(false);
  // Scroll automatico a fondo quando arriva una nuova riga di thinking.
  // Garantisce che il pannello mostri sempre l-ultimo pensiero, sia con il
  // blocco collassato (preview limitato) sia espanso.
  const preRef = useRef<HTMLPreElement | null>(null);
  useEffect(() => {
    const el = preRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [text, expanded]);

  /* Mostra solo le ultime 4 righe se collassato */
  const lines = text.split("\n");
  const preview = lines.length > 4 ? lines.slice(-4).join("\n") : text;

  return (
    <div
      style={{
        padding: "10px 14px",
        borderRadius: 10,
        background: tc.bgCard,
        border: `1px solid ${tc.border}`,
        alignSelf: "flex-start",
        maxWidth: "80%",
        fontSize: 13,
        lineHeight: 1.5,
      }}
    >
      {/* Intestazione cliccabile */}
      <div
        onClick={() => setExpanded((e) => !e)}
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          cursor: "pointer",
          userSelect: "none",
          marginBottom: 6,
        }}
      >
        <span
          style={{
            width: 8,
            height: 8,
            borderRadius: "50%",
            background: "#a78bfa",
            animation: "pulse 1.4s ease-in-out infinite",
            flexShrink: 0,
          }}
        />
        <span style={{ color: tc.textMuted, fontSize: 12, fontWeight: 600 }}>
          Ragionamento Nexus
        </span>
        <span style={{ color: tc.textMuted, fontSize: 11, opacity: 0.6 }}>
          {expanded ? "▲" : "▼"}
        </span>
      </div>

      {/* Contenuto */}
      <pre
        ref={preRef}
        style={{
          margin: 0,
          whiteSpace: "pre-wrap",
          wordBreak: "break-word",
          color: tc.textMuted,
          fontSize: 12,
          fontFamily: "inherit",
          maxHeight: expanded ? "none" : 100,
          overflow: expanded ? "visible" : "hidden",
          opacity: 0.85,
        }}
      >
        {expanded ? text : preview}
      </pre>

      {!expanded && lines.length > 4 && (
        <span
          onClick={() => setExpanded(true)}
          style={{
            color: "#a78bfa",
            fontSize: 11,
            cursor: "pointer",
            marginTop: 4,
            display: "inline-block",
          }}
        >
          Mostra tutto ({lines.length} righe)
        </span>
      )}
    </div>
  );
}

/* ------------------------------------------------------------------ */
/* AgentProgressInline  (P3)                                           */
/* ------------------------------------------------------------------ */

export function AgentProgressInline({
  tc,
  steps,
}: {
  tc: Record<string, string>;
  steps: AgentStep[];
}) {
  // Tempo dall'inizio del run (mount del componente). NON resettiamo ad ogni
  // step nuovo: con agenti che fanno step rapidi (<1s ognuno), il counter
  // restava bloccato a 0s confondendo l'utente. Ora avanza monotonicamente
  // per tutta la durata del run.
  const [elapsed, setElapsed] = useState(0);
  useEffect(() => {
    const id = setInterval(() => setElapsed((s) => s + 1), 1000);
    return () => clearInterval(id);
  }, []);

  const currentStep = steps[steps.length - 1];
  const recentDone = steps.filter((s) => s.status === "completed" || s.status === "failed").slice(-3);

  // toolLabel: punto unico in ./tool-labels (regola L).

  const statusIcon = (status: string) => {
    if (status === "completed") return "✓";
    if (status === "failed") return "✗";
    return "•";
  };

  const statusColor = (status: string) => {
    if (status === "completed") return "#22c55e";
    if (status === "failed") return tc.error || "#ef4444";
    return tc.textMuted;
  };

  // Badge avviso per step lenti
  let slowBadge: React.ReactNode = null;
  if (currentStep?.status === "running" && elapsed > 120) {
    slowBadge = (
      <span style={{ fontSize: 10, padding: "1px 6px", borderRadius: 4, background: "#ef444430", color: "#ef4444", fontWeight: 600 }}>
        &gt;2min
      </span>
    );
  } else if (currentStep?.status === "running" && elapsed > 30) {
    slowBadge = (
      <span style={{ fontSize: 10, padding: "1px 6px", borderRadius: 4, background: "#f9731630", color: "#f97316", fontWeight: 600 }}>
        &gt;30s
      </span>
    );
  }

  return (
    <div
      style={{
        padding: "10px 14px",
        borderRadius: 8,
        background: tc.bgCard,
        border: `1px solid ${tc.border}`,
        alignSelf: "stretch",
        fontSize: 12,
      }}
    >
      {/* Intestazione */}
      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: recentDone.length > 0 ? 6 : 0 }}>
        <span
          style={{
            width: 8,
            height: 8,
            borderRadius: "50%",
            background: "#22c55e",
            animation: "pulse 1.4s ease-in-out infinite",
            flexShrink: 0,
          }}
        />
        <span style={{ fontWeight: 600, color: tc.text }}>
          Nexus sta lavorando&hellip;
        </span>
        <span style={{ color: tc.textMuted }}>
          {toolLabel(currentStep?.toolName || "...")}
        </span>
        <span style={{ color: tc.textMuted, opacity: 0.7, fontSize: 11 }}>
          {elapsed}s
        </span>
        {slowBadge}
      </div>

      {/* Step recenti completati */}
      {recentDone.length > 0 && (
        <div style={{ display: "flex", flexDirection: "column", gap: 2, marginLeft: 16 }}>
          {recentDone.map((s) => (
            <div key={s.stepIndex} style={{ display: "flex", alignItems: "center", gap: 6, color: tc.textMuted }}>
              <span style={{ color: statusColor(s.status), fontSize: 11, fontWeight: 700 }}>
                {statusIcon(s.status)}
              </span>
              <span>{s.stepIndex + 1}. {toolLabel(s.toolName)}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
