"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { getProjectServicesStatus, getOutputEvents } from "../../lib/api-client";
import { promptFromDebugEntry } from "../../lib/chat-prompts";

export type DebugLevel = "ERROR" | "WARN" | "INFO" | "DEBUG";

export interface DebugEntry {
  id: string;
  level: DebugLevel;
  timestamp: string;
  message: string;
  raw: string;
  source?: string;
  expanded?: boolean;
}

export interface DebugPanelProps {
  terminalLines?: string[];
  projectId?: string;
  onSendToChat?: (message: string) => void;
}

const LOG_PATTERNS: Array<{
  pattern: RegExp;
  level: DebugLevel;
}> = [
  { pattern: /\[ERROR\]|\berror\s*TS\d+\b|^Error:|^\s+Error:|\bException\b|\bFAIL|FATAL|panicked|\bfailed\b|\[vite\][^\n]*\berror\b|\bERR_|Internal server error/i, level: "ERROR" },
  { pattern: /\[WARN\]|\bWarning:|\bwarn\b|\[vite\][^\n]*\bwarn/i, level: "WARN" },
  { pattern: /\[INFO\]|\binfo:/i, level: "INFO" },
  { pattern: /\[DEBUG\]|\bdebug:/i, level: "DEBUG" },
  { pattern: /^\s+at\s+\S+[\s.(]/, level: "ERROR" },
];

function classifyLine(line: string): DebugLevel | null {
  for (const { pattern, level } of LOG_PATTERNS) {
    if (pattern.test(line)) return level;
  }
  return null;
}

// Rimuove TUTTE le sequenze di escape ANSI/VT100, non solo SGR.
// La regex precedente (`\x1b\[[0-9;]*[mGKHFJABCDSTu]`) ignorava le sequenze CSI
// private come bracketed-paste (`\x1b[?2004h` / `\x1b[?2004l`): il `?` non era
// in `[0-9;]` e i terminatori `h`/`l` non erano nella classe finale, percio'
// nel Console Debug comparivano residui tipo `[?2004h` o `B[?20041`.
// Pattern costruito da escape \u (niente byte di controllo letterali nel
// sorgente): cattura CSI con byte privati/intermedi, OSC (terminato da BEL) e
// le designazioni di charset (`\x1b(B`, `\x1b)0`, ...).
const ANSI_PATTERN = new RegExp(
  "[\\u001B\\u009B][[\\]()#;?]*" +
    "(?:" +
    "(?:(?:[a-zA-Z\\d]*(?:;[a-zA-Z\\d]*)*)?\\u0007)" + // OSC ... BEL
    "|(?:\\d{1,4}(?:;\\d{0,4})*)?[\\dA-PR-TZcf-ntqry=><~]" + // CSI finale
    "|[()][AB0-9]" + // designazione charset G0/G1
    ")",
  "g",
);

function stripAnsi(str: string): string {
  return str.replace(ANSI_PATTERN, "");
}

let globalIdCounter = 0;

function parseLines(lines: string[], source?: string): DebugEntry[] {
  const entries: DebugEntry[] = [];

  for (const raw of lines) {
    const clean = stripAnsi(raw.trim());
    if (!clean) continue;
    const level = classifyLine(clean) || (source && source !== "terminal" ? "INFO" : null);
    if (!level) continue;

    const tsMatch = /^(\d{4}-\d{2}-\d{2}T?\d{2}:\d{2}:\d{2})/.exec(clean);
    const timestamp = tsMatch
      ? tsMatch[1].replace("T", " ").slice(11)
      : new Date().toLocaleTimeString("it-IT");

    entries.push({
      id: `dbg-${++globalIdCounter}`,
      level,
      timestamp,
      message: clean,
      raw: clean,
      source,
      expanded: false,
    });
  }
  return entries;
}

const LEVEL_COLORS: Record<DebugLevel, string> = {
  ERROR: "#f87171",
  WARN: "#fbbf24",
  INFO: "#60a5fa",
  DEBUG: "#94a3b8",
};

const LEVEL_ICONS: Record<DebugLevel, string> = {
  ERROR: "✕",
  WARN: "⚠",
  INFO: "ℹ",
  DEBUG: "•",
};

/** Righe vicine nella lista Debug (ordine di arrivo) per stack/eccezioni multi-riga */
const DEBUG_CHAT_CTX_BEFORE = 15;
const DEBUG_CHAT_CTX_AFTER = 15;
const DEBUG_CHAT_CTX_MAX = 50;

function adjacentDebugLines(entries: DebugEntry[], focusId: string): string[] {
  const idx = entries.findIndex((e) => e.id === focusId);
  if (idx < 0) return [];
  const start = Math.max(0, idx - DEBUG_CHAT_CTX_BEFORE);
  const end = Math.min(entries.length, idx + DEBUG_CHAT_CTX_AFTER + 1);
  const out: string[] = [];
  for (let j = start; j < end; j++) {
    if (j === idx) continue;
    const line = (entries[j].raw || entries[j].message).trimEnd();
    if (line) out.push(line);
    if (out.length >= DEBUG_CHAT_CTX_MAX) break;
  }
  return out;
}

type SourceFilter = "all" | "terminal" | string;

export function DebugPanel({ projectId, terminalLines, onSendToChat }: DebugPanelProps) {
  const tc = useThemeColors();
  const [entries, setEntries] = useState<DebugEntry[]>([]);
  const [filters, setFilters] = useState<Record<DebugLevel, boolean>>({
    ERROR: true,
    WARN: true,
    INFO: true,
    DEBUG: true,
  });
  const [sourceFilter, setSourceFilter] = useState<SourceFilter>("all");
  const [serviceNames, setServiceNames] = useState<string[]>([]);
  const lineBufferRef = useRef<string[]>([]);
  const seenLogIdsRef = useRef<Set<string>>(new Set());
  const pollTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Carica lista servizi del progetto. Includiamo, oltre agli `active`:
  //  - `managed_by === 'detached'` (WSL: systemd --user non attivo, l'unit gira
  //    con `setsid nohup ...` e scrive il log in /tmp/nexus-proj-<unit>.log);
  //  - `managed_by === 'windows'` (Windows nativo: i servizi sono processi
  //    gestiti in agent_processes, MAI unit systemd). Qui vanno inclusi ANCHE i
  //    servizi non-active/failed: il loro output di crash e' catturato in
  //    agent_processes.output/error_output e il backend lo espone via il canale
  //    `svc:` (logs.rs windows_service_log_events). Filtrarli via -> serviceNames
  //    vuoto -> Console Debug muta proprio quando serve diagnosticare il crash.
  useEffect(() => {
    if (!projectId) return;
    let active = true;
    getProjectServicesStatus(projectId)
      .then((res) => {
        if (!active) return;
        const names = res.services
          .filter((s: { state: string; managed_by?: string }) =>
            s.state === "active" ||
            s.managed_by === "detached" ||
            s.managed_by === "windows"
          )
          .map((s: { unit: string }) => s.unit);
        setServiceNames(names);
      })
      .catch(() => {});
    return () => { active = false; };
  }, [projectId]);

  // Polling log journalctl dei servizi
  useEffect(() => {
    if (!projectId || serviceNames.length === 0) return;

    const fetchServiceLogs = async () => {
      for (const unit of serviceNames) {
        try {
          const res = await getOutputEvents(projectId, `svc:${unit}`, 30);
          if (!res.events || res.events.length === 0) continue;

          const shortName = unit
            .replace(/\.service$/, "")
            .replace(/^[^-]+-/, "");

          const newEntries: DebugEntry[] = [];
          for (const ev of res.events) {
            // P4: chiave composta (id + createdAt + prefisso testo). ev.id
            // non e' garantito univoco dal backend: con la sola id le righe
            // NUOVE con id duplicato venivano scartate. Cosi' anche le righe
            // post-restart (createdAt diverso) non si perdono.
            const dedupKey = `${ev.id}|${ev.createdAt ?? ""}|${(ev.text || "").slice(0, 50)}`;
            if (seenLogIdsRef.current.has(dedupKey)) continue;
            seenLogIdsRef.current.add(dedupKey);

            const lines = (ev.text || "").split(/\r?\n/);
            const parsed = parseLines(lines, shortName);
            if (parsed.length > 0) {
              newEntries.push(...parsed);
            } else if (ev.level === "error" || ev.level === "warn") {
              newEntries.push({
                id: `svc-${ev.id}`,
                level: ev.level === "error" ? "ERROR" : "WARN",
                timestamp: ev.createdAt
                  ? new Date(ev.createdAt).toLocaleTimeString("it-IT")
                  : new Date().toLocaleTimeString("it-IT"),
                message: ev.title || ev.text,
                raw: ev.text,
                source: shortName,
                expanded: false,
              });
            }
          }

          if (newEntries.length > 0) {
            setEntries((prev) => [...prev, ...newEntries].slice(-800));
          }
        } catch {
          // ignora errori di fetch
        }
      }
    };

    fetchServiceLogs();
    pollTimerRef.current = setInterval(fetchServiceLogs, 5000);

    return () => {
      if (pollTimerRef.current) clearInterval(pollTimerRef.current);
    };
  }, [projectId, serviceNames]);

  // Righe dal terminale (prop terminalLines, accumulata incrementalmente dal
  // BottomPanelManager). L'array cresce nel tempo: processiamo SOLO le righe
  // nuove rispetto all'ultima volta per evitare di riclassificare e duplicare
  // tutto lo storico ad ogni cambiamento.
  const processedTerminalRef = useRef(0);
  useEffect(() => {
    const lines = terminalLines ?? [];
    // Se l'array si e' accorciato (reset al cambio progetto nel manager),
    // riparti da zero.
    if (lines.length < processedTerminalRef.current) {
      processedTerminalRef.current = 0;
    }
    const fresh = lines.slice(processedTerminalRef.current);
    processedTerminalRef.current = lines.length;
    if (fresh.length === 0) return;
    const parsed = parseLines(fresh, "terminal");
    if (parsed.length > 0) {
      setEntries((prev) => [...prev, ...parsed].slice(-800));
    }
  }, [terminalLines]);

  // Nota: il pannello Debug NON apre una propria connessione WebSocket al
  // terminale. Ogni connessione a /ws/terminal/{sid} crea una shell PTY
  // dedicata lato brain (brain/grpc_server/routes/terminal.py), quindi una
  // connessione qui vedrebbe una shell vuota e diversa da quella visibile.
  // L'output della shell visibile arriva invece tramite la prop terminalLines,
  // sollevata dal BottomPanelManager (callback onOutput del TerminalPanel).

  const toggleFilter = useCallback((level: DebugLevel) => {
    setFilters((prev) => ({ ...prev, [level]: !prev[level] }));
  }, []);

  const clearEntries = useCallback(() => {
    setEntries([]);
    lineBufferRef.current = [];
    seenLogIdsRef.current.clear();
  }, []);
  const sendEntryToChat = useCallback(
    (entry: DebugEntry) => {
      if (!onSendToChat) return;
      if (entry.level !== "ERROR" && entry.level !== "WARN") return;
      const contextLines = adjacentDebugLines(entries, entry.id);
      onSendToChat(
        promptFromDebugEntry({
          level: entry.level,
          timestamp: entry.timestamp,
          source: entry.source,
          message: entry.raw || entry.message,
          contextLines: contextLines.length > 0 ? contextLines : undefined,
        }),
      );
    },
    [onSendToChat, entries],
  );

  const toggleExpand = useCallback((id: string) => {
    setEntries((prev) =>
      prev.map((e) => (e.id === id ? { ...e, expanded: !e.expanded } : e)),
    );
  }, []);

  const allSources = Array.from(new Set(entries.map((e) => e.source).filter(Boolean))) as string[];

  const visible = entries.filter((e) => {
    if (!filters[e.level]) return false;
    if (sourceFilter === "all") return true;
    if (sourceFilter === "terminal") return e.source === "terminal" || !e.source;
    return e.source === sourceFilter;
  });

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        height: "100%",
        minHeight: 0,
        overflow: "hidden",
      }}
    >
      {/* Header */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 12,
          padding: "6px 12px",
          borderBottom: `1px solid ${tc.border}`,
          background: tc.bgHeader,
          flexShrink: 0,
          flexWrap: "wrap",
        }}
      >
        {(["ERROR", "WARN", "INFO", "DEBUG"] as DebugLevel[]).map((level) => (
          <label
            key={level}
            style={{
              display: "inline-flex",
              alignItems: "center",
              gap: 5,
              cursor: "pointer",
              fontSize: 11,
              color: filters[level] ? LEVEL_COLORS[level] : tc.textMuted,
              userSelect: "none",
            }}
          >
            <input
              type="checkbox"
              checked={filters[level]}
              onChange={() => toggleFilter(level)}
              style={{ accentColor: LEVEL_COLORS[level] }}
            />
            {level}
          </label>
        ))}

        {allSources.length > 0 && (
          <>
            <span style={{ color: tc.border, fontSize: 11 }}>|</span>
            <select
              value={sourceFilter}
              onChange={(e) => setSourceFilter(e.target.value)}
              style={{
                background: tc.bgInput,
                color: tc.text,
                border: `1px solid ${tc.border}`,
                borderRadius: 4,
                fontSize: 11,
                padding: "2px 6px",
                cursor: "pointer",
              }}
            >
              <option value="all">Tutte le sorgenti</option>
              <option value="terminal">Terminale</option>
              {allSources
                .filter((s) => s !== "terminal")
                .map((s) => (
                  <option key={s} value={s}>
                    {s}
                  </option>
                ))}
            </select>
          </>
        )}

        <div style={{ flex: 1 }} />
        {serviceNames.length > 0 && (
          <span style={{ fontSize: 10, color: tc.textMuted }}>
            {serviceNames.length} servizi monitorati
          </span>
        )}
        <button
          type="button"
          onClick={clearEntries}
          title="Cancella log"
          style={{
            background: "transparent",
            border: `1px solid ${tc.border}`,
            color: tc.textMuted,
            borderRadius: 6,
            padding: "2px 10px",
            cursor: "pointer",
            fontSize: 11,
          }}
        >
          Clear
        </button>
      </div>

      {/* Log list */}
      <div
        style={{
          flex: 1,
          minHeight: 0,
          overflow: "auto",
          padding: "6px 0",
          fontFamily: 'var(--font-mono)',
          fontSize: 12,
        }}
      >
        {visible.length === 0 ? (
          <div
            style={{
              padding: "20px 16px",
              color: tc.textMuted,
              fontSize: 12,
            }}
          >
            Nessun output di debug.
            {serviceNames.length > 0
              ? " I log dei servizi vengono caricati automaticamente."
              : " Avvia un processo nel terminale."}
          </div>
        ) : (
          visible.map((entry) => (
            <div
              key={entry.id}
              onClick={() => toggleExpand(entry.id)}
              style={{
                display: "flex",
                alignItems: "flex-start",
                gap: 8,
                padding: "4px 12px",
                cursor: "pointer",
                borderBottom: `1px solid ${tc.border}22`,
                background: entry.expanded ? `${LEVEL_COLORS[entry.level]}0d` : "transparent",
              }}
            >
              <span
                style={{
                  color: LEVEL_COLORS[entry.level],
                  fontSize: 13,
                  lineHeight: 1.4,
                  flexShrink: 0,
                  width: 14,
                  textAlign: "center",
                }}
              >
                {LEVEL_ICONS[entry.level]}
              </span>
              {entry.source && (
                <span
                  style={{
                    fontSize: 9,
                    padding: "1px 5px",
                    borderRadius: 3,
                    background: entry.source === "terminal" ? "#60a5fa22" : "#a78bfa22",
                    color: entry.source === "terminal" ? "#60a5fa" : "#a78bfa",
                    flexShrink: 0,
                    lineHeight: 1.8,
                    fontWeight: 600,
                    textTransform: "uppercase",
                  }}
                >
                  {entry.source}
                </span>
              )}
              <span
                style={{
                  color: tc.textMuted,
                  fontSize: 11,
                  flexShrink: 0,
                  lineHeight: 1.6,
                }}
              >
                {entry.timestamp}
              </span>
              <span
                style={{
                  color:
                    entry.level === "ERROR"
                      ? LEVEL_COLORS.ERROR
                      : entry.level === "WARN"
                        ? LEVEL_COLORS.WARN
                        : tc.text,
                  lineHeight: 1.6,
                  wordBreak: "break-all",
                  whiteSpace: entry.expanded ? "pre-wrap" : "nowrap",
                  overflow: entry.expanded ? "visible" : "hidden",
                  textOverflow: entry.expanded ? "unset" : "ellipsis",
                  flex: 1,
                  minWidth: 0,
                }}
              >
                {entry.message}
              </span>
              {onSendToChat && (entry.level === "ERROR" || entry.level === "WARN") && (
                <button
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    sendEntryToChat(entry);
                  }}
                  title={entry.level === "WARN"
                    ? "Apre la chat, invia subito e avvia l’agente in modalità conferma (patch + tool)"
                    : "Apre la chat, invia subito e avvia l’agente in modalità conferma (patch + tool)"}
                  style={{
                    marginLeft: 8,
                    background: entry.level === "WARN" ? "rgba(245,158,11,0.90)" : "rgba(239,68,68,0.85)",
                    color: entry.level === "WARN" ? "#111827" : "#fff",
                    border: "none",
                    borderRadius: 3,
                    padding: "0 6px",
                    fontSize: 10,
                    cursor: "pointer",
                    verticalAlign: "middle",
                    lineHeight: "16px",
                    height: 16,
                    fontWeight: entry.level === "WARN" ? 700 : 600,
                    flexShrink: 0,
                  }}
                >
                  ↗ chat
                </button>
              )}
            </div>
          ))
        )}
      </div>
    </div>
  );
}
