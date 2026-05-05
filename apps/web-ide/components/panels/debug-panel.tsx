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
  { pattern: /\[ERROR\]|\berror\s*TS\d+\b|^Error:|^\s+Error:|\bException\b|\bFAIL|FATAL|panicked|\bfailed\b/i, level: "ERROR" },
  { pattern: /\[WARN\]|\bWarning:|\bwarn\b/i, level: "WARN" },
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

function stripAnsi(str: string): string {
  return str.replace(/\x1b\[[0-9;]*[mGKHFJABCDSTu]/g, "");
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

const NEURAL_WS = (
  process.env.NEXT_PUBLIC_NEURAL_URL || "http://localhost:8001"
).replace(/^http/, "ws");

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
  const wsRef = useRef<WebSocket | null>(null);
  const lineBufferRef = useRef<string[]>([]);
  const seenLogIdsRef = useRef<Set<string>>(new Set());
  const pollTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);

  // Carica lista servizi del progetto
  useEffect(() => {
    if (!projectId) return;
    let active = true;
    getProjectServicesStatus(projectId)
      .then((res) => {
        if (!active) return;
        const names = res.services
          .filter((s: { state: string }) => s.state === "active")
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
            if (seenLogIdsRef.current.has(ev.id)) continue;
            seenLogIdsRef.current.add(ev.id);

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

  // Righe da prop
  useEffect(() => {
    if (!terminalLines || terminalLines.length === 0) return;
    const parsed = parseLines(terminalLines, "terminal");
    if (parsed.length > 0) {
      setEntries((prev) => [...prev, ...parsed].slice(-800));
    }
  }, [terminalLines]);

  // WebSocket terminale
  useEffect(() => {
    if (!projectId) return;

    const url = `${NEURAL_WS}/ws/terminal/${projectId}`;
    let active = true;

    const connect = () => {
      try {
        const ws = new WebSocket(url);
        wsRef.current = ws;

        ws.onmessage = (event) => {
          if (!active) return;
          try {
            const data =
              typeof event.data === "string"
                ? event.data
                : JSON.parse(event.data as string);
            const text =
              typeof data === "string"
                ? data
                : typeof data.output === "string"
                  ? data.output
                  : "";
            if (!text) return;
            const lines = text.split(/\r?\n/);
            lineBufferRef.current = [...lineBufferRef.current, ...lines].slice(
              -2000,
            );
            const parsed = parseLines(lines, "terminal");
            if (parsed.length > 0) {
              setEntries((prev) => [...prev, ...parsed].slice(-800));
            }
          } catch {
            // ignora errori di parsing
          }
        };

        ws.onerror = () => {};
      } catch {
        // ignora errori di connessione
      }
    };

    connect();

    return () => {
      active = false;
      wsRef.current?.close();
      wsRef.current = null;
    };
  }, [projectId]);

  const toggleFilter = useCallback((level: DebugLevel) => {
    setFilters((prev) => ({ ...prev, [level]: !prev[level] }));
  }, []);

  const clearEntries = useCallback(() => {
    setEntries([]);
    lineBufferRef.current = [];
    seenLogIdsRef.current.clear();
  }, []);
  const sendEntryToChat = useCallback((entry: DebugEntry) => {
    if (!onSendToChat) return;
    if (entry.level !== "ERROR" && entry.level !== "WARN") return;
    onSendToChat(promptFromDebugEntry({
      level: entry.level,
      timestamp: entry.timestamp,
      source: entry.source,
      message: entry.raw || entry.message,
    }));
  }, [onSendToChat]);

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
          fontFamily: '"JetBrains Mono", monospace',
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
                    ? "Invia questo warning alla chat di Nexus"
                    : "Invia questo errore alla chat di Nexus"}
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
