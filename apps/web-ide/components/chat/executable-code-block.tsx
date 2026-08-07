"use client";

import { useState, useCallback, useEffect, useRef } from "react";
import type { CSSProperties } from "react";
import { executeProjectCommand } from "../../lib/api-client";
import { useI18n } from "../../lib/i18n";

type ExecState = "idle" | "running" | "success" | "error" | "blocked";

interface CachedExec {
  state: ExecState;
  output: string;
  exitCode: number | null;
  durationMs: number | null;
}

const execCache = new Map<string, CachedExec>();
function cacheKey(projectId: string, code: string) {
  return `${projectId}:${code}`;
}

interface Props {
  code: string;
  language: string;
  projectId: string;
  tc: Record<string, string>;
}

// Icone SVG inline (niente dipendenze esterne)
function PlayIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" fill="currentColor">
      <path d="M4 2l10 6-10 6V2z" />
    </svg>
  );
}
function SpinnerIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="2">
      <circle cx="8" cy="8" r="6" strokeDasharray="28" strokeDashoffset="8">
        <animateTransform attributeName="transform" type="rotate" from="0 8 8" to="360 8 8" dur="0.8s" repeatCount="indefinite" />
      </circle>
    </svg>
  );
}
function CheckIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" fill="currentColor">
      <path d="M6.5 12.5l-4-4 1.4-1.4 2.6 2.6 5.6-5.6 1.4 1.4z" />
    </svg>
  );
}
function ErrorIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" fill="currentColor">
      <path d="M12.7 4.7l-1.4-1.4L8 6.6 4.7 3.3 3.3 4.7 6.6 8l-3.3 3.3 1.4 1.4L8 9.4l3.3 3.3 1.4-1.4L9.4 8z" />
    </svg>
  );
}
function BlockIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 16 16" fill="currentColor">
      <path d="M8 1a7 7 0 100 14A7 7 0 008 1zm0 2a5 5 0 013.5 8.6L4.4 4.5A5 5 0 018 3zm0 10a5 5 0 01-3.5-8.6l7.1 7.1A5 5 0 018 13z" />
    </svg>
  );
}
function CopyIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 16 16" fill="currentColor">
      <path d="M5 2a1 1 0 00-1 1v1H3a1 1 0 00-1 1v8a1 1 0 001 1h7a1 1 0 001-1v-1h1a1 1 0 001-1V3a1 1 0 00-1-1H5zm5 3H6a1 1 0 00-1 1v5H4V5h6V5zm1-1V3H5v1h6zm-1 3v5H5V7h5z" />
    </svg>
  );
}

const stateColors: Record<ExecState, string> = {
  idle: "#888",
  running: "#3b82f6",
  success: "#22c55e",
  error: "#ef4444",
  blocked: "#f59e0b",
};

const stateLabels: Record<ExecState, string> = {
  idle: "Esegui",
  running: "In esecuzione...",
  success: "Completato",
  error: "Errore",
  blocked: "Bloccato",
};

export function ExecutableCodeBlock({ code, language, projectId, tc }: Props) {
  const { t } = useI18n();
  const ck = cacheKey(projectId, code);
  const cached = execCache.get(ck);

  const [state, setState] = useState<ExecState>(
    cached && cached.state !== "running" ? cached.state : "idle",
  );
  const [output, setOutput] = useState(cached?.output ?? "");
  const [exitCode, setExitCode] = useState<number | null>(cached?.exitCode ?? null);
  const [durationMs, setDurationMs] = useState<number | null>(cached?.durationMs ?? null);
  const [showOutput, setShowOutput] = useState(cached ? cached.output !== "" : false);
  const [copied, setCopied] = useState(false);

  const runningRef = useRef(false);

  useEffect(() => {
    if (state !== "idle" && state !== "running") {
      execCache.set(ck, { state, output, exitCode, durationMs });
    }
  }, [ck, state, output, exitCode, durationMs]);

  const handleExecute = useCallback(async () => {
    if (state === "running" || runningRef.current) return;
    runningRef.current = true;
    setState("running");
    setOutput("");
    setExitCode(null);
    setDurationMs(null);
    setShowOutput(true);

    try {
      const result = await executeProjectCommand(projectId, code);
      setDurationMs(result.duration_ms);
      if (result.blocked) {
        setState("blocked");
        setOutput(result.stderr || result.blocked_reason || "Bloccato");
        setExitCode(-1);
      } else {
        const combined = [result.stdout, result.stderr].filter(Boolean).join("\n");
        setOutput(combined || "(nessun output)");
        setExitCode(result.exit_code);
        setState(result.exit_code === 0 ? "success" : "error");
      }
    } catch (err) {
      setState("error");
      setOutput(err instanceof Error ? err.message : "Errore di connessione");
      setExitCode(-1);
    } finally {
      runningRef.current = false;
    }
  }, [code, projectId, state]);

  const handleCopy = useCallback(() => {
    navigator.clipboard.writeText(code).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  }, [code]);

  const headerStyle: CSSProperties = {
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    padding: "4px 10px",
    background: tc.bgInput,
    borderBottom: `1px solid ${tc.border}`,
    borderRadius: "6px 6px 0 0",
    fontSize: 11,
    fontFamily: 'var(--font-mono)',
    gap: 6,
  };

  const btnStyle = (color: string): CSSProperties => ({
    display: "inline-flex",
    alignItems: "center",
    gap: 4,
    padding: "3px 8px",
    border: `1px solid ${color}`,
    borderRadius: 4,
    background: "transparent",
    color,
    fontSize: 11,
    fontFamily: "inherit",
    cursor: state === "running" ? "wait" : "pointer",
    transition: "background 0.15s",
  });

  const StateIcon = {
    idle: PlayIcon,
    running: SpinnerIcon,
    success: CheckIcon,
    error: ErrorIcon,
    blocked: BlockIcon,
  }[state];

  return (
    <div
      style={{
        border: `1px solid ${tc.border}`,
        borderRadius: 6,
        margin: "12px 0",
        overflow: "hidden",
      }}
    >
      {/* Header */}
      <div style={headerStyle}>
        <span style={{ color: tc.textSecondary, textTransform: "uppercase", letterSpacing: 0.5 }}>
          {language}
        </span>
        <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
          {durationMs !== null && (
            <span style={{ color: tc.textSecondary, fontSize: 10 }}>
              {durationMs < 1000 ? `${durationMs}ms` : `${(durationMs / 1000).toFixed(1)}s`}
            </span>
          )}
          {exitCode !== null && state !== "blocked" && (
            <span
              style={{
                color: exitCode === 0 ? "#22c55e" : "#ef4444",
                fontSize: 10,
                fontWeight: 600,
              }}
            >
              exit {exitCode}
            </span>
          )}
          <button
            type="button"
            title={t("chat.copiaComando")}
            onClick={handleCopy}
            style={btnStyle(tc.textSecondary)}
            onMouseEnter={(e) => { e.currentTarget.style.background = tc.bgHover ?? "#f3f3f3"; }}
            onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; }}
          >
            <CopyIcon />
            {copied ? "Copiato" : "Copia"}
          </button>
          <button
            type="button"
            title={stateLabels[state]}
            onClick={handleExecute}
            disabled={state === "running"}
            style={btnStyle(stateColors[state])}
            onMouseEnter={(e) => {
              if (state !== "running") e.currentTarget.style.background = `${stateColors[state]}18`;
            }}
            onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; }}
          >
            <StateIcon />
            {stateLabels[state]}
          </button>
        </div>
      </div>

      {/* Codice */}
      <pre
        style={{
          background: tc.bgInput,
          padding: "10px 12px",
          overflowX: "auto",
          fontFamily: 'var(--font-mono)',
          fontSize: 12,
          lineHeight: 1.5,
          color: tc.text,
          margin: 0,
          whiteSpace: "pre",
          borderRadius: 0,
          borderBottom: showOutput && output ? `1px solid ${tc.border}` : "none",
        }}
      >
        <code style={{ fontFamily: "inherit", fontSize: "inherit", color: "inherit" }}>
          {code}
        </code>
      </pre>

      {/* Output */}
      {showOutput && output && (
        <div>
          <button
            type="button"
            onClick={() => setShowOutput((v) => !v)}
            style={{
              width: "100%",
              textAlign: "left",
              padding: "4px 10px",
              background: state === "error" || state === "blocked"
                ? `${stateColors[state]}10`
                : `${stateColors.success}08`,
              border: "none",
              borderBottom: `1px solid ${tc.border}`,
              color: tc.textSecondary,
              fontSize: 10,
              fontFamily: 'var(--font-mono)',
              cursor: "pointer",
              letterSpacing: 0.3,
            }}
          >
            {t("badge.output")}
          </button>
          <pre
            style={{
              background: "#1a1a2e",
              color: "#e8e8e8",
              padding: "8px 12px",
              margin: 0,
              fontSize: 11.5,
              lineHeight: 1.5,
              fontFamily: 'var(--font-mono)',
              overflowX: "auto",
              maxHeight: 300,
              overflowY: "auto",
              whiteSpace: "pre-wrap",
              wordBreak: "break-word",
              borderRadius: "0 0 6px 6px",
            }}
          >
            {output}
          </pre>
        </div>
      )}
    </div>
  );
}
