"use client";

import { useState, useEffect, useCallback, } from "react";
import { useThemeColors } from "../../lib/theme";
import { ServiceMetricsStrip } from "./service-metrics-strip";
import { getOutputChannels, getOutputEvents, stopAgentProcess, clearFinishedProcesses, type OutputChannel, type OutputEvent } from "../../lib/api-client";
import {
  useProjectStore,
  selectOutputChannelsChangedAt,
  selectServicesMap,
} from "../../lib/project-dispatcher/store";

interface ServicesPanelProps {
  projectId: string;
  projectName?: string;
  onSendToChat?: (message: string) => void;
}

/** Stile condiviso per i bottoni d'azione della toolbar (Apri/Invia/Stop).
 *  Punto unico (regola L / ADR 0026) per evitare il clone 44L intra-file. */
function actionBtnStyle(background: string): React.CSSProperties {
  return {
    background,
    color: "#fff",
    border: "none",
    borderRadius: 4,
    padding: "2px 10px",
    fontSize: 11,
    cursor: "pointer",
    flexShrink: 0,
    display: "flex",
    alignItems: "center",
    gap: 4,
  };
}

export function ServicesPanel({ projectId, projectName, onSendToChat }: ServicesPanelProps) {
  const tc = useThemeColors();
  const [processes, setProcesses] = useState<OutputChannel[]>([]);
  const [activeTab, setActiveTab] = useState<string>("");
  const [output, setOutput] = useState<OutputEvent | null>(null);
  const [loading, setLoading] = useState(false);
  const [hiddenTabs, setHiddenTabs] = useState<Set<string>>(() => {
    try {
      const stored = typeof window !== "undefined"
        ? localStorage.getItem(`nexus:services-hidden:${projectId}`)
        : null;
      return stored ? new Set(JSON.parse(stored) as string[]) : new Set();
    } catch { return new Set(); }
  });
  // Persist hidden tabs to localStorage (sopravvive alla chiusura del browser)
  useEffect(() => {
    try {
      localStorage.setItem(`nexus:services-hidden:${projectId}`, JSON.stringify([...hiddenTabs]));
    } catch { /* ignore */ }
  }, [hiddenTabs, projectId]);

  // Fetch process list
  const fetchProcesses = useCallback(async () => {
    try {
      const res = await getOutputChannels(projectId);
      const agentChannels = (res.channels ?? []).filter((ch: OutputChannel) =>
        ch.id.startsWith("agent:")
      );
      setProcesses(agentChannels);
      // Auto-nasconde i task effimeri e i servizi terminati non appena smettono di girare
      setHiddenTabs((prev) => {
        const done = agentChannels.filter(
          (ch) => !ch.label.startsWith("●")
        );
        if (done.length === 0) return prev;
        const next = new Set(prev);
        for (const ch of done) next.add(ch.id);
        return next;
      });
      setActiveTab((current) => {
        // Calcola i processi visibili con i dati aggiornati
        const visible = agentChannels.filter(
          (p) => p.label.startsWith("●") || !hiddenTabs.has(p.id)
        );
        if (visible.length === 0) return "";
        // Se il tab corrente è ancora visibile, mantienilo
        if (current && visible.some((p) => p.id === current)) return current;
        // Altrimenti seleziona il primo running, poi il primo in assoluto
        const running = visible.find((p) => p.label.startsWith("●"));
        return running?.id ?? visible[0].id;
      });
    } catch {
      // ignore
    }
  }, [projectId, hiddenTabs]);

  // Fetch output for active tab
  const fetchOutput = useCallback(async () => {
    if (!activeTab) return;
    setLoading(true);
    try {
      const res = await getOutputEvents(projectId, activeTab, 1);
      setOutput(res.events?.[0] ?? null);
    } catch {
      // ignore
    } finally {
      setLoading(false);
    }
  }, [projectId, activeTab]);

  // Event-driven: ricarica canali quando OutputChannelCreated arriva via SSE
  const outputChannelsChangedAt = useProjectStore(selectOutputChannelsChangedAt);
  useEffect(() => {
    if (outputChannelsChangedAt > 0) void fetchProcesses();
  }, [outputChannelsChangedAt, fetchProcesses]);

  // Event-driven: ricarica canali quando lo stato di un servizio cambia (start/stop/restart)
  const servicesMap = useProjectStore(selectServicesMap);
  useEffect(() => {
    if (servicesMap && Object.keys(servicesMap).length > 0) void fetchProcesses();
  }, [servicesMap, fetchProcesses]);

  // Fetch iniziale + fallback polling rilassato (30s)
  useEffect(() => {
    fetchProcesses();
    const interval = setInterval(fetchProcesses, 30_000);
    return () => clearInterval(interval);
  }, [fetchProcesses]);

  useEffect(() => {
    if (activeTab) {
      // Resetta l'output precedente quando si cambia tab per evitare flash di dati vecchi
      setOutput(null);
      fetchOutput();
      const interval = setInterval(fetchOutput, 3000);
      return () => clearInterval(interval);
    } else {
      setOutput(null);
    }
  }, [activeTab, fetchOutput]);

  const isFinished = (label: string) => label.startsWith("✗") || label.startsWith("○");
  const isRunning = (label: string) => label.startsWith("●");

  // I processi running sono sempre visibili (ignorano hiddenTabs)
  const visibleProcesses = processes.filter((p) => isRunning(p.label) || !hiddenTabs.has(p.id));
  const activeProc = visibleProcesses.find((p) => p.id === activeTab) ?? null;

  // Riscrive localhost / 127.0.0.1 / 0.0.0.0 con l'host effettivo del server (quello da cui l'utente sta accedendo)
  const rewriteHost = (url: string): string => {
    if (typeof window === "undefined") return url;
    return url.replace(
      /(https?:\/\/)(localhost|127\.0\.0\.1|0\.0\.0\.0)(:\d+)?/gi,
      (_m, scheme: string, _host: string, port: string | undefined) => {
        return `${scheme}${window.location.hostname}${port || ""}`;
      },
    );
  };

  // Estrae la prima URL http(s) dal testo (preferisce quelle con porta)
  const extractFirstUrl = (text: string): string | null => {
    const re = /https?:\/\/[^\s<>"')]+/gi;
    const matches = text.match(re);
    if (!matches || matches.length === 0) return null;
    // Preferisci quelle che contengono "localhost" o "127" o ":<port>" (probabili dev server)
    const preferred = matches.find((u) => /(localhost|127\.0\.0\.1|0\.0\.0\.0|:\d{2,5})/i.test(u));
    return rewriteHost(preferred || matches[0]);
  };

  const previewUrl = output?.text ? extractFirstUrl(output.text) : null;

  const openInBrowser = () => {
    if (!previewUrl) return;
    window.open(previewUrl, "_blank", "noopener,noreferrer");
  };

  // Renderizza il testo con URL cliccabili (e host riscritto)
  const renderOutputText = (text: string): React.ReactNode => {
    const re = /(https?:\/\/[^\s<>"')]+)/gi;
    const parts: React.ReactNode[] = [];
    let lastIndex = 0;
    let match: RegExpExecArray | null;
    let key = 0;
    while ((match = re.exec(text)) !== null) {
      if (match.index > lastIndex) {
        parts.push(text.slice(lastIndex, match.index));
      }
      const original = match[0];
      const rewritten = rewriteHost(original);
      parts.push(
        <a
          key={`url-${key++}`}
          href={rewritten}
          target="_blank"
          rel="noopener noreferrer"
          style={{ color: tc.accent, textDecoration: "underline", wordBreak: "break-all" }}
        >
          {rewritten}
        </a>,
      );
      lastIndex = match.index + original.length;
    }
    if (lastIndex < text.length) {
      parts.push(text.slice(lastIndex));
    }
    return parts;
  };

  const sendErrorToChat = () => {
    if (!onSendToChat || !output) return;
    const proc = activeProc;
    const procLabelClean = proc?.label?.replace(/^[●○✗]\s*/, "") ?? "(processo)";
    // Estrai stato/exit code dal title se disponibile (es. "dotnet run [pid: 656872, status: failed, exit: 134] — ...")
    const titleMeta = output.title || "";
    // Tronca l'output agli ultimi 200 righe per stare nei limiti del context, mantenendo gli errori finali
    const lines = (output.text || "").split("\n");
    const tail = lines.length > 200 ? lines.slice(-200) : lines;
    const truncatedNote = lines.length > 200 ? `\n[...output troncato: mostrate ultime 200 righe su ${lines.length}]` : "";
    const outputBlock = tail.join("\n");

    const contextParts: string[] = [];
    if (projectName) contextParts.push(`**Progetto:** ${projectName}`);
    contextParts.push(`**Servizio:** \`${procLabelClean}\``);
    if (titleMeta) contextParts.push(`**Stato:** ${titleMeta}`);
    contextParts.push(`**Timestamp:** ${new Date(output.createdAt).toLocaleString()}`);

    const msg = `Ho ricevuto questo errore di runtime durante l'esecuzione del servizio. Analizza il problema, individua la causa root e proponi una soluzione (modificando i file se necessario).

${contextParts.join("\n")}

**Output / log:**
\`\`\`
${outputBlock}${truncatedNote}
\`\`\``;

    onSendToChat(msg);
  };

  const closeTab = (id: string) => {
    setHiddenTabs((prev) => new Set(prev).add(id));
    if (activeTab === id) {
      const remaining = visibleProcesses.filter((p) => p.id !== id);
      setActiveTab(remaining[0]?.id ?? "");
    }
  };

  if (visibleProcesses.length === 0) {
    return (
      <div style={{ padding: 16, color: tc.textMuted, fontSize: 13 }}>
        Nessun servizio attivo. Usa la chat per chiedere all&apos;agente di avviare servizi.
      </div>
    );
  }

  const hasFinished = processes.some((p) => isFinished(p.label));

  const handleClearFinished = async () => {
    try {
      await clearFinishedProcesses(projectId);
      setHiddenTabs(new Set());
      await fetchProcesses();
    } catch { /* ignore */ }
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%", minHeight: 0 }}>
      {/* Osservabilita runtime dei servizi utente (service_observer) */}
      <ServiceMetricsStrip projectId={projectId} />
      {/* Tab bar */}
      <div
        style={{
          display: "flex",
          gap: 0,
          borderBottom: `1px solid ${tc.border}`,
          flexShrink: 0,
          overflowX: "auto",
          background: tc.bgSidebar,
        }}
      >
        <div style={{ flex: 1, display: "flex", overflowX: "auto" }}>
        {visibleProcesses.map((proc) => {
          const isActive = proc.id === activeTab;
          const canClose = isFinished(proc.label);
          return (
            <div
              key={proc.id}
              style={{ display: "flex", alignItems: "center", flexShrink: 0 }}
            >
              <button
                onClick={() => setActiveTab(proc.id)}
                style={{
                  padding: "6px 14px",
                  paddingRight: canClose ? "4px" : "14px",
                  fontSize: 12,
                  fontFamily: '"JetBrains Mono", monospace',
                  background: isActive ? tc.bgCard : "transparent",
                  color: isActive ? tc.accent : tc.textMuted,
                  border: "none",
                  borderBottom: isActive ? `2px solid ${tc.accent}` : "2px solid transparent",
                  cursor: "pointer",
                  whiteSpace: "nowrap",
                }}
              >
                {proc.label}
              </button>
              {canClose && (
                <button
                  onClick={(e) => { e.stopPropagation(); closeTab(proc.id); }}
                  title="Chiudi"
                  style={{
                    background: "none",
                    border: "none",
                    color: tc.textMuted,
                    cursor: "pointer",
                    padding: "4px 6px",
                    fontSize: 14,
                    lineHeight: 1,
                    borderBottom: isActive ? `2px solid ${tc.accent}` : "2px solid transparent",
                  }}
                >
                  ×
                </button>
              )}
            </div>
          );
        })}
        </div>
        {hasFinished && (
          <button
            onClick={handleClearFinished}
            title="Elimina tutti i processi terminati"
            style={{
              flexShrink: 0, background: "none", border: "none",
              borderLeft: `1px solid ${tc.border}`,
              color: tc.textMuted, cursor: "pointer",
              padding: "0 10px", fontSize: 11, whiteSpace: "nowrap",
            }}
          >
            🗑 Pulisci
          </button>
        )}
      </div>

      {/* Info bar (fixed) */}
      {output && (
        <div style={{
          display: "flex", justifyContent: "space-between", alignItems: "center",
          padding: "4px 10px", borderBottom: `1px solid ${tc.border}`, flexShrink: 0,
          fontFamily: '"JetBrains Mono", monospace', fontSize: 11, color: tc.textMuted,
        }}>
          <span>{output.title} &mdash; {new Date(output.createdAt).toLocaleString()}</span>
          <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
            {previewUrl && (
              <button
                onClick={openInBrowser}
                title={`Apri ${previewUrl} in una nuova scheda`}
                style={actionBtnStyle("#10b981")}
              >
                <span>🌐</span> Apri
              </button>
            )}
            {activeTab && output && onSendToChat && (
              <button
                onClick={sendErrorToChat}
                title="Invia output e contesto a Nexus per analizzare l'errore"
                style={actionBtnStyle(tc.accent)}
              >
                <span>💬</span> Invia a Nexus
              </button>
            )}
            {activeTab && activeProc && isRunning(activeProc.label) && (
              <button
                onClick={async () => {
                  const procId = activeTab.replace("agent:", "");
                  try {
                    await stopAgentProcess(projectId, procId);
                    fetchOutput();
                    fetchProcesses();
                  } catch { /* ignore */ }
                }}
                style={{ ...actionBtnStyle(tc.error), display: "inline-block", gap: undefined }}
              >
                Stop
              </button>
            )}
            {activeTab && activeProc && isFinished(activeProc.label) && (
              <button
                onClick={() => closeTab(activeTab)}
                style={{
                  background: "none", color: tc.textMuted, border: `1px solid ${tc.border}`, borderRadius: 4,
                  padding: "2px 10px", fontSize: 11, cursor: "pointer", flexShrink: 0,
                }}
              >
                Chiudi
              </button>
            )}
          </div>
        </div>
      )}

      {/* Output area (scrollable) */}
      <div
        style={{
          flex: 1,
          minHeight: 0,
          overflow: "auto",
          padding: 10,
          fontFamily: '"JetBrains Mono", monospace',
          fontSize: 12,
          background: tc.bgCard,
        }}
      >
        {loading && !output ? (
          <div className="text-muted">Caricamento...</div>
        ) : output ? (
          <pre
            style={{
              whiteSpace: "pre-wrap",
              wordBreak: "break-word",
              margin: 0,
              color: output.level === "error" ? tc.error : tc.text,
            }}
          >
            {output.text ? renderOutputText(output.text) : "(nessun output)"}
          </pre>
        ) : (
          <div className="text-muted">Seleziona un servizio per vederne l&apos;output.</div>
        )}
      </div>
    </div>
  );
}
