"use client";

/**
 * OutputPanel — pannello unificato Output + Servizi.
 *
 * Colonna sinistra: tutti i canali disponibili (System, Git, Tasks, …)
 *   + processi agente attivi/terminati (prefisso "agent:").
 * Area destra: contenuto del canale selezionato.
 *   - Canali statici: polling 5s (comportamento precedente).
 *   - Canali agent:  SSE in tempo reale, autoscroll, colori ANSI base.
 *
 * Funzionalità agente: Stop, Invia a Nexus, Apri URL, Chiudi (solo terminati).
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import {
  getOutputChannels,
  getOutputEvents,
  stopAgentProcess,
  clearFinishedProcesses,
  type OutputChannel,
  type OutputEvent,
} from "../../lib/api-client";

interface OutputPanelProps {
  projectId: string;
  projectName?: string;
  /** Canali statici già caricati dal parent (System, Git, Tasks, …). */
  staticChannels?: OutputChannel[];
  /** Events per il canale statico selezionato (dal parent via polling). */
  staticEvents?: OutputEvent[];
  selectedStaticChannel?: string;
  onSelectStaticChannel?: (id: string) => void;
  onClear?: () => void;
  onSendToChat?: (message: string) => void;
}

// ── Helpers ANSI ────────────────────────────────────────────────────────────

const ANSI_COLORS: Record<number, string> = {
  30: "#1e1e1e", 31: "#ef4444", 32: "#22c55e", 33: "#f59e0b",
  34: "#3b82f6", 35: "#a855f7", 36: "#06b6d4", 37: "#d1d5db",
  90: "#6b7280", 91: "#f87171", 92: "#4ade80", 93: "#fbbf24",
  94: "#60a5fa", 95: "#c084fc", 96: "#22d3ee", 97: "#f9fafb",
};

function ansiToNodes(text: string): React.ReactNode[] {
  // Supporto minimo: colori foreground + reset
  const re = /\x1b\[([0-9;]*)m/g;
  const nodes: React.ReactNode[] = [];
  let lastIdx = 0;
  let currentColor: string | undefined;
  let match: RegExpExecArray | null;

  while ((match = re.exec(text)) !== null) {
    if (match.index > lastIdx) {
      const chunk = text.slice(lastIdx, match.index);
      nodes.push(
        <span key={lastIdx} style={currentColor ? { color: currentColor } : undefined}>
          {chunk}
        </span>
      );
    }
    const codes = match[1].split(";").map(Number);
    for (const code of codes) {
      if (code === 0) { currentColor = undefined; }
      else if (ANSI_COLORS[code]) { currentColor = ANSI_COLORS[code]; }
    }
    lastIdx = match.index + match[0].length;
  }
  if (lastIdx < text.length) {
    nodes.push(
      <span key={lastIdx} style={currentColor ? { color: currentColor } : undefined}>
        {text.slice(lastIdx)}
      </span>
    );
  }
  return nodes;
}

function linkify(text: string): React.ReactNode[] {
  const re = /(https?:\/\/[^\s<>"')]+)/gi;
  const parts: React.ReactNode[] = [];
  let lastIdx = 0;
  let match: RegExpExecArray | null;
  let key = 0;
  while ((match = re.exec(text)) !== null) {
    if (match.index > lastIdx) parts.push(text.slice(lastIdx, match.index));
    const url = match[0].replace(
      /(https?:\/\/)(localhost|127\.0\.0\.1|0\.0\.0\.0)(:\d+)?/gi,
      (_m, s: string, _h: string, p?: string) =>
        typeof window !== "undefined"
          ? `${s}${window.location.hostname}${p ?? ""}`
          : match![0],
    );
    parts.push(
      <a key={key++} href={url} target="_blank" rel="noopener noreferrer"
        style={{ color: "#3b82f6", textDecoration: "underline", wordBreak: "break-all" }}>
        {url}
      </a>
    );
    lastIdx = match.index + match[0].length;
  }
  if (lastIdx < text.length) parts.push(text.slice(lastIdx));
  return parts;
}

// ── Detection righe d'errore ─────────────────────────────────────────────
// Importante: matchiamo SOLO la riga d'apertura di un errore, NON le righe
// dello stack trace (che spesso contengono "Exception" come parte del nome
// pacchetto, es. `at Microsoft...EntityFrameworkException...`). Senza questa
// distinzione metà dei log sembrava errore.
//
// I log syslog/journalctl arrivano con prefisso `<timestamp> <host> <unit>: <msg>`.
// Quindi NON ancoriamo a `^`: cerchiamo il pattern d'errore preceduto da
// word-boundary o whitespace, per matchare anche dopo il prefisso.
//
// Pattern di "riga d'apertura":
//   .NET ILogger:  fail: ...
//   .NET Exception: <pkg>.<Class>Exception: messaggio
//   Python:        Traceback (most recent call last):
//   Rust:          panicked at ... / panic: ...
//   Go:            <stack>panic: ...
//   Generic logs:  [ERROR] / ERROR / FATAL / [FATAL]
const ERROR_HEADER_RES: RegExp[] = [
  /(?:^|\s)fail:\s/,                                              // .NET ILogger: "... fail: ..."
  /(?:^|\s)Traceback\s*\(most recent call last\)\s*:?\s*$/,        // Python
  /(?:^|\s)[\w.$]*(?:Exception|Error|Fault):\s+\S/,                // "... System.ArgumentException: Couldn't..."
  /(?:^|\s)panicked at\s/,                                          // Rust
  /(?:^|\s)panic:\s/,                                               // Go
  /(?:^|\s)\[?(?:ERROR|FATAL|FAIL)\]?[\s:]/,                        // "... [ERROR] msg" / "... FATAL: msg"
  /(?:^|\s)Uncaught\s+\w*Error\b/,                                  // JS uncaught
  /(?:^|\s)--->\s+[\w.$]*(?:Exception|Error|Fault):/,               // .NET inner exception ("---> System.X.Exception:")
];

// Pattern righe stack trace (NON evidenziare anche se contengono "Exception").
// Anche qui no `^`: il prefisso syslog c'è anche prima di "at ".
// Match esempio: "Apr 26 ... bash[123]:        at Microsoft.X.Y.Method(...)"
const STACK_FRAME_RE = /(?:^|\s)(?:at\s+[A-Z][\w$.]*|File\s+"[^"]+",\s+line\s+\d+|---\s+End of\s|in\s+\S+\.(?:cs|ts|tsx|py|rs|go|java|kt):\s*(?:line\s+)?\d+)/;

const WARN_LINE_RE  = /(?:^|\s)\[?(?:WARN|WARNING)\]?[\s:]|deprecated/i;

function classifyLine(line: string): "error" | "warn" | null {
  // Stack frames hanno priorità: NON evidenziarli, sono "contesto" non causa
  if (STACK_FRAME_RE.test(line)) return null;
  if (ERROR_HEADER_RES.some(re => re.test(line))) return "error";
  if (WARN_LINE_RE.test(line))  return "warn";
  return null;
}

// Strip prefisso syslog/journalctl per applicare i pattern di stack frame
// a righe come "2026-04-30T10:24:50+02:00 Dino dotnet[6705]:    at MyMethod()".
// Senza questo, le righe di continuazione di un errore non vengono incluse
// nel blocco perche' il prefisso le fa sembrare "non indentate".
const SYSLOG_PREFIX_RES: RegExp[] = [
  // ISO short: "2026-04-30T10:24:50+02:00 host process[pid]: "
  /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:[+-]\d{2}:?\d{2}|Z)?\s+\S+\s+[^:]+:\s?/,
  // Classic syslog: "Apr 26 12:34:56 host process[pid]: "
  /^[A-Z][a-z]{2}\s+\d{1,2}\s+\d{2}:\d{2}:\d{2}\s+\S+\s+[^:]+:\s?/,
];
function stripSyslogPrefix(line: string): string {
  for (const re of SYSLOG_PREFIX_RES) {
    const m = line.match(re);
    if (m) return line.slice(m[0].length);
  }
  return line;
}

/** Estrae il blocco di contesto attorno a una riga d'errore: la riga stessa
 *  + tutte le righe successive che sono stack frame, fino a max 20 righe.
 *  Usato dal bottone "↗" per spedire il singolo errore alla chat con contesto. */
export function extractErrorBlock(text: string, errorLineIdx: number, maxFollowing = 20): string {
  const clean = text.replace(/\x1b\[[0-9;]*m/g, "");
  const lines = clean.split("\n");
  if (errorLineIdx < 0 || errorLineIdx >= lines.length) return "";
  const block: string[] = [lines[errorLineIdx]];
  for (let j = errorLineIdx + 1; j < lines.length && block.length < maxFollowing + 1; j++) {
    const line = lines[j];
    if (line.trim() === "") {
      // Riga vuota: la consideriamo separatore "soft" — la includiamo se la
      // successiva e' ancora stack frame, altrimenti chiudiamo il blocco.
      const next = lines[j + 1] ?? "";
      const stripped = stripSyslogPrefix(next);
      const continues = STACK_FRAME_RE.test(stripped)
        || /^\s+\S/.test(stripped)
        || /^\s*--->/.test(stripped);
      if (continues) { block.push(line); continue; }
      break;
    }
    // Strip prefisso syslog (se presente) prima di testare i pattern: questo
    // permette di includere stack trace e inner exceptions anche quando il
    // log proviene da journalctl/syslog con timestamp+host+process[pid] davanti.
    const stripped = stripSyslogPrefix(line);
    const isStack = STACK_FRAME_RE.test(stripped) || STACK_FRAME_RE.test(line);
    const isIndented = /^\s+\S/.test(stripped);
    const isInner = /^\s*--->/.test(stripped);
    if (isStack || isIndented || isInner) {
      block.push(line);
    } else {
      break;
    }
  }
  return block.join("\n");
}

interface RenderRichTextOptions {
  onSendErrorToChat?: (singleErrorBlock: string) => void;
}

function renderRichText(text: string, opts: RenderRichTextOptions = {}): React.ReactNode {
  // Rimuovi escape ANSI
  const clean = text.replace(/\x1b\[[0-9;]*m/g, "");
  const lines = clean.split("\n");
  return lines.map((line, i) => {
    const kind = classifyLine(line);
    const bg =
      kind === "error" ? "rgba(239,68,68,0.12)" :
      kind === "warn"  ? "rgba(245,158,11,0.10)" : "transparent";
    const borderLeft =
      kind === "error" ? "2px solid #ef4444" :
      kind === "warn"  ? "2px solid #f59e0b" : "none";
    // Le righe di inner exception (.NET "--->") fanno già parte del blocco
    // emesso dal pulsante della riga root: niente secondo pulsante.
    const isInnerException = /(?:^|\s)--->\s/.test(line);
    const showSendBtn = (kind === "error" || kind === "warn") && !isInnerException && !!opts.onSendErrorToChat;
    const sendBtnStyle =
      kind === "warn"
        ? {
            marginLeft: 8,
            background: "rgba(245,158,11,0.90)",
            color: "#111827",
            border: "none",
            borderRadius: 3,
            padding: "0 6px",
            fontSize: 10,
            cursor: "pointer",
            verticalAlign: "middle",
            lineHeight: "16px",
            height: 16,
            fontWeight: 700,
          }
        : {
            marginLeft: 8,
            background: "rgba(239,68,68,0.85)",
            color: "#fff",
            border: "none",
            borderRadius: 3,
            padding: "0 6px",
            fontSize: 10,
            cursor: "pointer",
            verticalAlign: "middle",
            lineHeight: "16px",
            height: 16,
            fontWeight: 600,
          };
    return (
      <span key={i} style={{
        display: "block",
        background: bg,
        borderLeft,
        paddingLeft: kind ? 6 : 0,
        marginLeft: kind ? -8 : 0,
        position: "relative",
      }}>
        {linkify(line)}
        {showSendBtn && (
          <button
            onClick={(e) => {
              e.stopPropagation();
              const block = extractErrorBlock(text, i);
              opts.onSendErrorToChat?.(block);
            }}
            title={kind === "warn"
              ? "Invia SOLO questo warning (con contesto) alla chat di Nexus"
              : "Invia SOLO questo errore (con stack trace) alla chat di Nexus"}
            style={sendBtnStyle}
          >
            ↗ chat
          </button>
        )}
        {i < lines.length - 1 ? "\n" : ""}
      </span>
    );
  });
}

/** Estrae fino a `max` blocchi d'errore (riga d'apertura + stack trace conseguente).
 *  Usato dal bottone "🔥 N errori" per il riepilogo gerarchico. */
export function extractErrorLines(text: string, max = 30): string[] {
  const clean = text.replace(/\x1b\[[0-9;]*m/g, "");
  const lines = clean.split("\n");
  const out: string[] = [];
  for (let i = 0; i < lines.length && out.length < max; i++) {
    if (classifyLine(lines[i]) === "error") {
      out.push(extractErrorBlock(clean, i));
    }
  }
  return out;
}


// ── Componente principale ─────────────────────────────────────────────────

export function OutputPanel({
  projectId,
  projectName,
  staticChannels = [],
  staticEvents = [],
  selectedStaticChannel = "",
  onSelectStaticChannel,
  onClear,
  onSendToChat,
}: OutputPanelProps) {
  const tc = useThemeColors();

  // --- Canali agente ---
  const [agentChannels, setAgentChannels] = useState<OutputChannel[]>([]);
  const [selectedAgent, setSelectedAgent] = useState<string>("");
  const [agentOutput, setAgentOutput] = useState<string>("");
  const [agentTitle, setAgentTitle] = useState<string>("");
  const [agentStatus, setAgentStatus] = useState<"running" | "stopped" | "failed" | "">("");
  const [agentTimestamp, setAgentTimestamp] = useState<string>("");
  const [hiddenAgents, setHiddenAgents] = useState<Set<string>>(new Set());
  const outputRef = useRef<HTMLDivElement>(null);
  const esRef = useRef<EventSource | null>(null);
  const [sseActive, setSseActive] = useState(false);

  // --- Selezione testo → Invia alla chat ---
  const [selection, setSelection] = useState<{ text: string; x: number; y: number } | null>(null);

  const handleMouseUp = useCallback(() => {
    const sel = window.getSelection();
    if (!sel || sel.isCollapsed) { setSelection(null); return; }
    const text = sel.toString().trim();
    if (!text || text.length < 3) { setSelection(null); return; }
    if (!outputRef.current) { setSelection(null); return; }
    const range = sel.getRangeAt(0);
    if (!outputRef.current.contains(range.commonAncestorContainer)) { setSelection(null); return; }
    const rect = range.getBoundingClientRect();
    const containerRect = outputRef.current.getBoundingClientRect();
    setSelection({
      text,
      x: Math.min(rect.right - containerRect.left, containerRect.width - 200),
      y: rect.bottom - containerRect.top + 4,
    });
  }, []);

  // Determina se è attivo un canale agente o statico
  const activeIsAgent = selectedAgent !== "";

  const handleSendSelection = useCallback(() => {
    if (!selection || !onSendToChat) return;
    const channel = activeIsAgent ? agentTitle : (selectedStaticChannel || "System");
    const msg = `Analizza questo estratto dal canale **${channel}** e proponi soluzioni se si tratta di un errore:

\`\`\`
${selection.text}
\`\`\``;
    onSendToChat(msg);
    setSelection(null);
    window.getSelection()?.removeAllRanges();
  }, [selection, onSendToChat, activeIsAgent, agentTitle, selectedStaticChannel]);

  // --- Fetch canali agente ---
  const fetchAgentChannels = useCallback(async () => {
    try {
      const res = await getOutputChannels(projectId);
      const agents = (res.channels ?? []).filter((ch: OutputChannel) =>
        ch.id.startsWith("agent:")
      );
      setAgentChannels(agents);
      // Auto-nasconde processi terminati
      setHiddenAgents((prev) => {
        const next = new Set(prev);
        for (const ch of agents) {
          if (!ch.label.startsWith("●")) next.add(ch.id);
        }
        // Riporta in vita quelli che tornano running
        for (const ch of agents) {
          if (ch.label.startsWith("●")) next.delete(ch.id);
        }
        return next;
      });
      // Seleziona automaticamente il primo processo running se nessun agente è selezionato
      setSelectedAgent((current) => {
        if (current && agents.some((a) => a.id === current)) return current;
        const running = agents.find((a) => a.label.startsWith("●"));
        return running?.id ?? current;
      });
    } catch { /* ignore */ }
  }, [projectId]);

  useEffect(() => {
    fetchAgentChannels();
    const iv = setInterval(fetchAgentChannels, 5000);
    return () => clearInterval(iv);
  }, [fetchAgentChannels]);

  // --- SSE per canale agente selezionato ---
  const startSse = useCallback((processId: string) => {
    if (esRef.current) {
      esRef.current.close();
      esRef.current = null;
    }
    setAgentOutput("");
    setAgentTitle("");
    setAgentStatus("");
    setSseActive(true);

    const url = `/api/projects/${projectId}/agent-processes/${processId}/stream`;
    const es = new EventSource(url);
    esRef.current = es;

    es.onmessage = (e) => {
      try {
        const data = JSON.parse(e.data as string) as {
          type: string; text: string; status: string;
        };
        if (data.text) {
          setAgentOutput((prev) => prev + data.text);
          // Autoscroll in fondo
          requestAnimationFrame(() => {
            if (outputRef.current) {
              outputRef.current.scrollTop = outputRef.current.scrollHeight;
            }
          });
        }
        if (data.status) {
          setAgentStatus(data.status as "running" | "stopped" | "failed" | "");
        }
        if (data.type === "end") {
          setSseActive(false);
          es.close();
          esRef.current = null;
        }
      } catch { /* ignore */ }
    };
    es.onerror = () => {
      setSseActive(false);
      es.close();
      esRef.current = null;
    };
  }, [projectId]);

  // Fallback polling se SSE non è disponibile
  const fetchAgentOutputPolling = useCallback(async (channelId: string) => {
    try {
      const res = await getOutputEvents(projectId, channelId, 1);
      const ev = res.events?.[0];
      if (ev) {
        setAgentOutput(ev.text ?? "");
        setAgentTitle(ev.title ?? "");
        setAgentTimestamp(ev.createdAt ?? "");
      }
    } catch { /* ignore */ }
  }, [projectId]);

  useEffect(() => {
    if (!selectedAgent) return;
    const processId = selectedAgent.replace("agent:", "");
    // Carica snapshot iniziale via polling, poi avvia SSE
    void fetchAgentOutputPolling(selectedAgent);
    startSse(processId);
    return () => {
      if (esRef.current) { esRef.current.close(); esRef.current = null; }
    };
  }, [selectedAgent, startSse, fetchAgentOutputPolling]);

  // Aggiorna title/timestamp dal canale
  useEffect(() => {
    if (!selectedAgent) return;
    const ch = agentChannels.find((a) => a.id === selectedAgent);
    if (ch) setAgentTitle(ch.label.replace(/^[●○✗]\s*/, ""));
  }, [agentChannels, selectedAgent]);

  // Auto-scroll a fondo per canali statici (es. svc:* con journalctl).
  // I log arrivano in ordine cronologico ascendente (piu' vecchi in cima),
  // quindi all'apertura mostriamo subito il "tail" come fa `journalctl -f`.
  useEffect(() => {
    if (selectedAgent) return; // canali agent gestiscono lo scroll via SSE
    if (!outputRef.current) return;
    if (staticEvents.length === 0) return;
    requestAnimationFrame(() => {
      if (outputRef.current) {
        outputRef.current.scrollTop = outputRef.current.scrollHeight;
      }
    });
  }, [selectedAgent, selectedStaticChannel, staticEvents]);

  // --- Helpers UI ---
  const visibleAgents = agentChannels.filter(
    (a) => a.label.startsWith("●") || !hiddenAgents.has(a.id)
  );

  const extractFirstUrl = (text: string) => {
    const re = /https?:\/\/[^\s<>"')]+/gi;
    const matches = text.match(re);
    if (!matches) return null;
    const preferred = matches.find((u) => /(localhost|127\.0\.0\.1|0\.0\.0\.0|:\d{2,5})/i.test(u));
    const raw = preferred ?? matches[0];
    if (typeof window === "undefined") return raw;
    return raw.replace(
      /(https?:\/\/)(localhost|127\.0\.0\.1|0\.0\.0\.0)(:\d+)?/gi,
      (_m, s: string, _h: string, p?: string) =>
        `${s}${window.location.hostname}${p ?? ""}`,
    );
  };

  const previewUrl = agentOutput ? extractFirstUrl(agentOutput) : null;

  const handleStop = async () => {
    if (!selectedAgent) return;
    const pid = selectedAgent.replace("agent:", "");
    try {
      await stopAgentProcess(projectId, pid);
      await fetchAgentChannels();
    } catch { /* ignore */ }
  };

  const handleClearFinished = async () => {
    try {
      await clearFinishedProcesses(projectId);
      setHiddenAgents(new Set());
      setSelectedAgent("");
      setAgentOutput("");
      await fetchAgentChannels();
    } catch { /* ignore */ }
  };

  const handleSendToChat = () => {
    if (!onSendToChat || !agentOutput) return;
    const lines = agentOutput.split("\n");
    const tail = lines.length > 200 ? lines.slice(-200) : lines;
    const note = lines.length > 200 ? `\n[...troncato: ultime 200 righe su ${lines.length}]` : "";
    const msg = `Ho ricevuto questo output dal servizio. Analizza e proponi soluzioni se necessario.

**Progetto:** ${projectName ?? projectId}
**Servizio:** \`${agentTitle}\`

**Output:**
\`\`\`
${tail.join("\n")}${note}
\`\`\``;
    onSendToChat(msg);
  };

  // Testo concatenato del canale attivo (agente o statico) — usato per analisi errori e invio
  const activeChannelTitle = activeIsAgent
    ? agentTitle
    : (selectedStaticChannel || "System");

  const activeChannelText = activeIsAgent
    ? agentOutput
    : staticEvents.map(ev => `[${ev.title}]\n${ev.text}`).join("\n\n---\n\n");

  const errorBlocks = extractErrorLines(activeChannelText, 30);

  /** Invia alla chat UN SOLO errore (riga d'apertura + il suo stack trace).
   *  Usato dal bottoncino "↗ chat" affianco a ogni riga d'errore evidenziata. */
  const handleSendSingleError = useCallback((singleErrorBlock: string) => {
    if (!onSendToChat || !singleErrorBlock.trim()) return;
    const channel = activeChannelTitle;
    const msg = `Errore singolo dal canale **${channel}** del progetto **${projectName ?? projectId}**.

Analizza solo questo errore (è la riga d'apertura + il suo stack trace), individua la causa specifica nel codice del progetto, leggi i file coinvolti e proponi/applica una correzione minima. Poi proponi un restart del servizio per verificare.

\`\`\`
${singleErrorBlock}
\`\`\``;
    onSendToChat(msg);
  }, [onSendToChat, activeChannelTitle, projectName, projectId]);

  const handleSendErrors = () => {
    if (!onSendToChat || errorBlocks.length === 0) return;
    // Manda alla chat il log grezzo SENZA pre-analizzarlo: è l'AI di Nexus che
    // deve identificare la gerarchia degli errori, isolare la causa radice e
    // procedere a un fix per volta. Il prompt sotto la istruisce esplicitamente.
    const msg = `Il servizio **${activeChannelTitle}** del progetto **${projectName ?? projectId}** sta fallendo. Ti incollo il log degli ultimi ${errorBlocks.length} blocchi d'errore in ordine cronologico.

**Compito (rigorosamente in quest'ordine, NON saltare passi):**

1. **Analizza la gerarchia degli errori.** I log che ti mando contengono tipicamente molte ripetizioni dello stesso problema e/o errori a cascata. Identifica:
   - Quanti tipi DISTINTI di eccezione sono presenti (raggruppa per signature: nome eccezione + primo frame del progetto utente).
   - Quale è la **causa radice**: il primo errore in ordine cronologico, da cui tutti gli altri probabilmente discendono.
   - Quali sono **conseguenze** (errori secondari che spariranno una volta risolta la causa radice).

2. **Riepiloga l'analisi** in un blocco breve PRIMA di toccare codice, così:
   > Causa radice: \`<eccezione>\` in \`<file:line>\` — ricorre N volte.
   > Effetti a cascata: \`<altra_eccezione>\` × M, \`<altra2>\` × K.

3. **Concentrati SOLO sulla causa radice.** Non tentare di fixare gli errori secondari adesso: spariranno da soli quando la radice è risolta.

4. **Usa i tool per indagare e risolvere:**
   - \`read_file\` per leggere il file menzionato nello stack trace della causa radice.
   - Se il file non basta, cerca riferimenti correlati (config, env, ecc.).
   - Proponi (e applica con \`str_replace\`/\`write_file\`) la modifica MINIMA necessaria.

5. **Verifica** chiedendo un restart del servizio (es. \`nexus_service_control\` con action=restart) e riguarda i log per confermare che la causa radice è sparita.

6. Se compaiono nuovi errori dopo il restart, ricomincia dal punto 1 sui log freschi (NON dai log che ti mando ora).

**Log degli ultimi blocchi d'errore (con riga di contesto sopra/sotto ciascuno):**

\`\`\`
${errorBlocks.join("\n---\n")}
\`\`\``;
    onSendToChat(msg);
  };

  const handleSendStaticChannelToChat = () => {
    if (!onSendToChat || !activeChannelText) return;
    const lines = activeChannelText.split("\n");
    const tail = lines.length > 200 ? lines.slice(-200) : lines;
    const note = lines.length > 200 ? `\n[...troncato: ultime 200 righe su ${lines.length}]` : "";
    const msg = `Output del canale **${activeChannelTitle}** del progetto **${projectName ?? projectId}**.
Analizza e proponi soluzioni se necessario.

\`\`\`
${tail.join("\n")}${note}
\`\`\``;
    onSendToChat(msg);
  };

  const allStaticChannels = staticChannels.length
    ? staticChannels.filter((ch) => !ch.id.startsWith("agent:"))
    : [{ id: "System", label: "System" }];

  const hasFinishedAgents = agentChannels.some(
    (a) => a.label.startsWith("✗") || a.label.startsWith("○")
  );

  // Stili condivisi
  const sideBtn = (active: boolean) => ({
    width: "100%",
    minWidth: 0,
    textAlign: "left" as const,
    padding: "5px 10px",
    background: active ? (tc.accentBg ?? `${tc.accent}20`) : "transparent",
    color: active ? tc.accent : tc.text,
    border: "none",
    borderBottom: `1px solid ${tc.border}`,
    borderRadius: 0,
    cursor: "pointer",
    fontSize: 11,
    fontFamily: '"JetBrains Mono", monospace',
    display: "flex",
    alignItems: "center",
    gap: 4,
    overflow: "hidden",
  });

  return (
    <div style={{
      display: "grid",
      gridTemplateColumns: "minmax(160px, 200px) 1fr",
      height: "100%",
      minHeight: 0,
    }}>
      {/* ── Colonna sinistra: lista canali ── */}
      <div style={{
        borderRight: `1px solid ${tc.border}`,
        overflowY: "auto",
        display: "flex",
        flexDirection: "column",
        minHeight: 0,
      }}>
        {/* Canali statici */}
        {allStaticChannels.map((ch) => (
          <button
            key={ch.id}
            onClick={() => { setSelectedAgent(""); onSelectStaticChannel?.(ch.id); }}
            style={sideBtn(!activeIsAgent && selectedStaticChannel === ch.id)}
            title={ch.title ?? ch.label}
          >
            <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", width: "100%" }}>{ch.label}</span>
          </button>
        ))}

        {/* Separatore se ci sono processi agente */}
        {visibleAgents.length > 0 && (
          <div style={{
            padding: "4px 10px",
            fontSize: 10,
            color: tc.textMuted,
            textTransform: "uppercase",
            letterSpacing: "0.08em",
            borderBottom: `1px solid ${tc.border}`,
            borderTop: `1px solid ${tc.border}`,
            background: tc.bgSidebar,
            flexShrink: 0,
          }}>
            Servizi
          </div>
        )}

        {/* Canali agente */}
        {visibleAgents.map((ch) => {
          const isRunning = ch.label.startsWith("●");
          const isFailed = ch.label.startsWith("✗");
          const label = ch.label.replace(/^[●○✗]\s*/, "");
          const dot = isRunning ? "●" : isFailed ? "✗" : "○";
          const dotColor = isRunning ? "#22c55e" : isFailed ? "#ef4444" : "#6b7280";
          const isActive = selectedAgent === ch.id;
          return (
            <button
              key={ch.id}
              onClick={() => { setSelectedAgent(ch.id); onSelectStaticChannel?.(""); }}
              style={sideBtn(isActive)}
              title={label}
            >
              <span style={{ color: dotColor, flexShrink: 0, fontSize: 10 }}>{dot}</span>
              <span style={{ overflow: "hidden", textOverflow: "ellipsis", flex: 1 }}>{label}</span>
            </button>
          );
        })}

        {/* Pulsante pulisci terminati */}
        {hasFinishedAgents && (
          <button
            onClick={handleClearFinished}
            style={{
              background: "none",
              border: "none",
              borderTop: `1px solid ${tc.border}`,
              color: tc.textMuted,
              cursor: "pointer",
              padding: "6px 10px",
              fontSize: 11,
              textAlign: "left" as const,
              marginTop: "auto",
            }}
          >
            🗑 Pulisci terminati
          </button>
        )}
      </div>

      {/* ── Area destra: contenuto ── */}
      <div style={{ display: "flex", flexDirection: "column", minHeight: 0, height: "100%" }}>

        {/* Toolbar */}
        <div style={{
          display: "flex",
          justifyContent: "space-between",
          alignItems: "center",
          padding: "4px 8px",
          borderBottom: `1px solid ${tc.border}`,
          flexShrink: 0,
          gap: 6,
          background: tc.bgSidebar,
          minHeight: 30,
        }}>
          <span style={{
            fontSize: 11,
            color: tc.textMuted,
            fontFamily: '"JetBrains Mono", monospace',
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
            flex: 1,
          }}>
            {activeIsAgent
              ? agentTitle + (agentTimestamp ? ` — ${new Date(agentTimestamp).toLocaleTimeString("it-IT")}` : "") +
                (sseActive ? " ●" : "")
              : (selectedStaticChannel || "System")}
          </span>
          <div style={{ display: "flex", gap: 4, flexShrink: 0 }}>
            {activeIsAgent && previewUrl && (
              <button
                onClick={() => window.open(previewUrl, "_blank", "noopener,noreferrer")}
                style={{ background: "#10b981", color: "#fff", border: "none", borderRadius: 4, padding: "2px 8px", fontSize: 11, cursor: "pointer" }}
              >
                🌐 Apri
              </button>
            )}
            {activeIsAgent && onSendToChat && agentOutput && (
              <button
                onClick={handleSendToChat}
                style={{ background: tc.accent, color: "#fff", border: "none", borderRadius: 4, padding: "2px 8px", fontSize: 11, cursor: "pointer" }}
              >
                💬 Nexus
              </button>
            )}
            {!activeIsAgent && onSendToChat && staticEvents.length > 0 && (
              <button
                onClick={handleSendStaticChannelToChat}
                title={`Invia alla chat l'output del canale ${activeChannelTitle}`}
                style={{ background: tc.accent, color: "#fff", border: "none", borderRadius: 4, padding: "2px 8px", fontSize: 11, cursor: "pointer" }}
              >
                💬 Nexus
              </button>
            )}
            {onSendToChat && errorBlocks.length > 0 && (
              <button
                onClick={handleSendErrors}
                title={`Invia alla chat ${errorBlocks.length} blocchi di errore con contesto`}
                style={{ background: "#ef4444", color: "#fff", border: "none", borderRadius: 4, padding: "2px 8px", fontSize: 11, cursor: "pointer" }}
              >
                🔥 {errorBlocks.length} errori
              </button>
            )}
            {activeIsAgent && agentStatus === "running" && (
              <button
                onClick={handleStop}
                style={{ background: "#ef4444", color: "#fff", border: "none", borderRadius: 4, padding: "2px 8px", fontSize: 11, cursor: "pointer" }}
              >
                ■ Stop
              </button>
            )}
            {!activeIsAgent && (staticEvents.length > 0 || agentOutput) && (
              <button
                onClick={onClear}
                style={{ background: "none", border: `1px solid ${tc.border}`, borderRadius: 4, color: tc.textMuted, cursor: "pointer", padding: "2px 8px", fontSize: 11 }}
              >
                Clear
              </button>
            )}
          </div>
        </div>

        {/* Contenuto output */}
        <div
          ref={outputRef}
          onMouseUp={handleMouseUp}
          style={{
            flex: 1,
            minHeight: 0,
            overflow: "auto",
            padding: "8px 12px",
            fontFamily: '"JetBrains Mono", monospace',
            fontSize: 12,
            background: tc.bgCard,
            lineHeight: 1.5,
            position: "relative",
            userSelect: "text",
          }}
        >
          {selection && onSendToChat && (
            <button
              onClick={handleSendSelection}
              style={{
                position: "absolute",
                left: selection.x,
                top: selection.y,
                zIndex: 10,
                background: tc.accent,
                color: "#fff",
                border: "none",
                borderRadius: 4,
                padding: "4px 10px",
                fontSize: 11,
                cursor: "pointer",
                boxShadow: "0 2px 8px rgba(0,0,0,0.3)",
                whiteSpace: "nowrap",
              }}
              title="Invia il testo selezionato alla chat per analisi"
            >
              ↗ Invia alla chat
            </button>
          )}
          {activeIsAgent ? (
            agentOutput ? (
              <pre style={{ margin: 0, whiteSpace: "pre-wrap", wordBreak: "break-word", color: tc.text }}>
                {renderRichText(agentOutput, { onSendErrorToChat: handleSendSingleError })}
              </pre>
            ) : (
              <div style={{ color: tc.textMuted }}>
                {sseActive ? "In attesa di output…" : "Nessun output disponibile."}
              </div>
            )
          ) : (
            staticEvents.length === 0 ? (
              <div style={{ color: tc.textMuted }}>Nessun evento per il canale selezionato.</div>
            ) : (
              staticEvents.map((ev) => (
                <div key={ev.id} style={{ marginBottom: 12 }}>
                  <div style={{ fontWeight: 700, color: tc.text }}>{ev.title}</div>
                  <div style={{ color: tc.textMuted, fontSize: 11 }}>
                    {new Date(ev.createdAt).toLocaleString("it-IT")}
                  </div>
                  <pre style={{ whiteSpace: "pre-wrap", wordBreak: "break-word", margin: "4px 0 0", color: tc.text }}>
                    {renderRichText(ev.text, { onSendErrorToChat: handleSendSingleError })}
                  </pre>
                </div>
              ))
            )
          )}
        </div>
      </div>
    </div>
  );
}
