"use client";

import { useThemeColors } from "../../lib/theme";
import { TerminalPanel } from "../terminal-panel";
import { DebugPanel } from "./debug-panel";
import { OutputPanel } from "./output-panel";
import { OptimizationPanel } from "./optimization-panel";
import { RunPanel } from "./run-panel";
import type {
  AITraceEvent,
  OutputChannel,
  OutputEvent,
  PlaywrightRunSummary,
  PortEntry,
  ProblemItem,
  UserProjectDetails,
} from "../../lib/api-client";

export type PanelTab =
  | "problems"
  | "output"
  | "debug"
  | "run"
  | "terminal"
  | "services"
  | "ports"
  | "playwright"
  | "optimization";

export interface BottomPanelManagerProps {
  activePanelTab: PanelTab;
  project: UserProjectDetails | null;
  problemItems: ProblemItem[];
  outputChannels: OutputChannel[];
  selectedOutputChannel: string;
  outputEvents: OutputEvent[];
  ports: PortEntry[];
  playwrightRuns: PlaywrightRunSummary[];
  traces?: AITraceEvent[];
  onOpenFile: (path: string, line?: number) => void;
  onSelectOutputChannel: (id: string) => void;
  onClearPanel?: (tab: PanelTab) => void;
  onSendToChat?: (message: string) => void;
  onAutoSendToChat?: (message: string) => void;
  agentRunEndSignal?: number;
  onClearTraces?: () => void;
}

function listRowButton(tc: ReturnType<typeof useThemeColors>) {
  return {
    width: "100%",
    textAlign: "left",
    padding: "8px 10px",
    borderRadius: 8,
    border: `1px solid ${tc.border}`,
    background: tc.bgCard,
    cursor: "pointer",
    display: "flex",
    flexDirection: "column",
    gap: 4,
  } as const;
}

function tileStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    border: `1px solid ${tc.border}`,
    borderRadius: 8,
    background: tc.bgCard,
    padding: "10px 12px",
  } as const;
}

function severityColor(
  severity: string,
  tc: ReturnType<typeof useThemeColors>,
) {
  const n = severity.toLowerCase();
  if (n === "error" || n === "critical" || n === "high") return tc.error;
  if (n === "warning" || n === "medium") return tc.warning;
  return tc.textMuted;
}

export function BottomPanelManager({
  activePanelTab,
  project,
  problemItems,
  outputChannels,
  selectedOutputChannel,
  outputEvents,
  ports,
  playwrightRuns,
  traces = [],
  onOpenFile,
  onSelectOutputChannel,
  onClearPanel,
  onSendToChat,
  onAutoSendToChat,
  agentRunEndSignal,
  onClearTraces,
}: BottomPanelManagerProps) {
  const tc = useThemeColors();

  const clearBar = (tab: PanelTab, hasContent: boolean) =>
    hasContent ? (
      <div style={{ display: "flex", justifyContent: "flex-end", padding: "4px 8px", borderBottom: `1px solid ${tc.border}`, flexShrink: 0 }}>
        <button
          onClick={() => onClearPanel?.(tab)}
          title="Cancella contenuto"
          style={{
            background: "none",
            border: `1px solid ${tc.border}`,
            borderRadius: 4,
            color: tc.textMuted,
            cursor: "pointer",
            padding: "2px 8px",
            fontSize: 11,
          }}
        >
          Clear
        </button>
      </div>
    ) : null;

  // OptimizationPanel è SEMPRE montato (display:none quando non attivo).
  // Usare un early-return condizionale lo unmountenterebbe ad ogni cambio tab,
  // perdendo autoFixEnabled, pendingMarkOnNextRunRef e i segnali agentRunEndSignal
  // arrivati mentre il pannello era nascosto. Con display:none lo stato React resta vivo.
  const optimizationVisible = !project || activePanelTab === "optimization";

  // Pannello corrente (tutti gli altri tab usano conditional rendering normale)
  const otherPanel = () => {
    if (!project) return null;

    if (activePanelTab === "problems") return (
      <div style={{ display: "flex", flexDirection: "column", height: "100%", minHeight: 0 }}>
        {clearBar("problems", problemItems.length > 0)}
        <div style={{ display: "flex", flexDirection: "column", gap: 8, padding: 10, minHeight: 0, overflow: "auto", flex: 1 }}>
          {problemItems.length === 0 ? (
            <div style={{ color: tc.textMuted }}>Nessun problema aperto.</div>
          ) : (
            problemItems.map((item) => (
              <button
                key={item.id}
                onClick={() => item.filePath && onOpenFile(item.filePath, item.line ?? 1)}
                style={listRowButton(tc)}
              >
                <div style={{ display: "flex", justifyContent: "space-between", gap: 10 }}>
                  <span style={{ color: tc.text, fontSize: 12 }}>{item.message}</span>
                  <span style={{ color: severityColor(item.severity, tc), fontSize: 11 }}>{item.severity}</span>
                </div>
                <div style={{ color: tc.textMuted, fontSize: 11 }}>
                  {item.source}{item.filePath ? ` • ${item.filePath}${item.line ? `:${item.line}` : ""}` : ""}
                </div>
              </button>
            ))
          )}
        </div>
      </div>
    );

    // "services" (e alias legacy "output") usa OutputPanel
    if (activePanelTab === "services" || activePanelTab === "output") return (
      <OutputPanel
        projectId={project.id}
        projectName={project.name}
        staticChannels={outputChannels}
        staticEvents={outputEvents}
        selectedStaticChannel={selectedOutputChannel}
        onSelectStaticChannel={onSelectOutputChannel}
        onClear={() => onClearPanel?.("services")}
        onSendToChat={onSendToChat}
      />
    );

    if (activePanelTab === "debug") return <DebugPanel projectId={project.id} />;

    if (activePanelTab === "run") return (
      <RunPanel
        projectId={project.id}
        projectName={project.name}
        onSendToChat={onSendToChat}
        agentRunEndSignal={agentRunEndSignal}
      />
    );

    if (activePanelTab === "terminal") return (
      <TerminalPanel projectId={project.id} projectLabel={project.name} embedded />
    );

    if (activePanelTab === "ports") return (
      <div style={{ display: "flex", flexDirection: "column", height: "100%", minHeight: 0 }}>
        {clearBar("ports", ports.length > 0)}
        <div style={{ padding: 12, overflow: "auto", flex: 1, minHeight: 0 }}>
          {ports.length === 0 ? (
            <div style={{ color: tc.textMuted }}>Nessuna porta rilevata per il progetto.</div>
          ) : (
            ports.map((port, index) => (
              <div key={`${port.port ?? "port"}-${index}`} style={tileStyle(tc)}>
                <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                  <span style={{
                    background: tc.accentBg,
                    color: tc.accent,
                    borderRadius: 3,
                    padding: "1px 6px",
                    fontSize: 11,
                    fontFamily: "monospace",
                    flexShrink: 0,
                  }}>
                    {port.port}
                  </span>
                  <span style={{ color: tc.text, fontWeight: 500 }}>{port.label || `Porta ${port.port}`}</span>
                </div>
                {port.url ? (
                  <a
                    href={port.url}
                    target="_blank"
                    rel="noreferrer"
                    style={{
                      color: tc.accent ?? "#4a9eff",
                      fontSize: 11,
                      textDecoration: "none",
                      wordBreak: "break-all",
                    }}
                    onMouseEnter={e => (e.currentTarget.style.textDecoration = "underline")}
                    onMouseLeave={e => (e.currentTarget.style.textDecoration = "none")}
                  >
                    {port.url}
                  </a>
                ) : (
                  <div style={{ color: tc.textMuted, fontSize: 11 }}>Nessun URL disponibile</div>
                )}
              </div>
            ))
          )}
        </div>
      </div>
    );

    // playwright
    const handleRunPlaywright = () => {
      if (!onSendToChat) return;
      onSendToChat(
        "Esegui i test Playwright del progetto.\n" +
        "1. usa `run_command` con `pnpm exec playwright test --reporter=list 2>&1 | tail -60` nella directory del progetto\n" +
        "2. Riporta il risultato: quanti test passano, quanti falliscono, eventuali errori\n" +
        "3. Se tutti i test falliscono con 'cannot open shared object file', segnalalo chiaramente (dipendenze di sistema mancanti)"
      );
    };
    const handleEnablePlaywright = () => {
      if (!onSendToChat) return;
      onSendToChat(
        "Abilita Playwright nel progetto. Esegui questi step nell'ordine:\n" +
        "1. usa `run_command` per installare le dipendenze: `pnpm add -D @playwright/test` nella directory del progetto\n" +
        "2. usa `run_command` per installare i browser: `pnpm exec playwright install --with-deps chromium` nella directory del progetto\n" +
        "3. crea il file `playwright.config.ts` nella root del progetto con una configurazione base (baseURL http://localhost:3000, testDir e2e, solo chromium, retries 1, timeout 30s)\n" +
        "4. crea la directory `e2e/` con un file `e2e/example.spec.ts` che contiene un test minimale (navigazione alla home e verifica del titolo)\n" +
        "5. Riporta il risultato finale: conferma che Playwright e' abilitato e pronto, elenca i file creati"
      );
    };
    return (
      <div style={{ display: "flex", flexDirection: "column", height: "100%", minHeight: 0 }}>
        <div style={{
          display: "flex", alignItems: "center", justifyContent: "space-between",
          padding: "4px 10px", borderBottom: `1px solid ${tc.border}`, flexShrink: 0,
          gap: 8,
        }}>
          <span style={{ fontSize: 11, color: tc.textMuted }}>
            {playwrightRuns.length > 0 ? `${playwrightRuns.length} run` : "Nessun run"}
          </span>
          <div style={{ display: "flex", gap: 6 }}>
            {onSendToChat && (
              <button
                onClick={handleEnablePlaywright}
                title="Configura Playwright nel progetto tramite Nexus"
                style={{
                  background: "#6366f1", color: "#fff", border: "none", borderRadius: 4,
                  padding: "2px 10px", fontSize: 11, cursor: "pointer",
                  display: "flex", alignItems: "center", gap: 4,
                }}
              >
                Abilita Playwright
              </button>
            )}
            {onSendToChat && (
              <button
                onClick={handleRunPlaywright}
                title="Avvia i test Playwright tramite Nexus"
                style={{
                  background: "#10b981", color: "#fff", border: "none", borderRadius: 4,
                  padding: "2px 10px", fontSize: 11, cursor: "pointer",
                  display: "flex", alignItems: "center", gap: 4,
                }}
              >
                Avvia test
              </button>
            )}
            {playwrightRuns.length > 0 && (
              <button onClick={() => onClearPanel?.("playwright")} style={listRowButton(tc)}>
                Pulisci
              </button>
            )}
          </div>
        </div>
        <div style={{ padding: 12, overflow: "auto", flex: 1, minHeight: 0 }}>
          {playwrightRuns.length === 0 ? (
            <div style={{ color: tc.textMuted, fontSize: 12 }}>
              Nessun run Playwright disponibile. Premi <strong>Abilita Playwright</strong> per configurare il framework di test, poi <strong>Avvia test</strong> per eseguirli.
            </div>
          ) : (
            playwrightRuns.map((run) => (
              <div key={run.id} style={tileStyle(tc)}>
                <div style={{ display: "flex", justifyContent: "space-between", gap: 10 }}>
                  <span className="font-semibold" style={{ color: tc.text }}>{run.label}</span>
                  <span style={{
                    color: run.status === "passed" ? "#10b981" : run.status === "failed" ? tc.error : tc.textMuted,
                    fontSize: 11, fontWeight: 600,
                  }}>{run.status}</span>
                </div>
                {run.summary && <div style={{ color: tc.textSecondary, fontSize: 12, marginTop: 6 }}>{run.summary}</div>}
                <div style={{ color: tc.textMuted, fontSize: 11, marginTop: 6 }}>{new Date(run.createdAt).toLocaleString()}</div>
              </div>
            ))
          )}
        </div>
      </div>
    );
  };

  return (
    <>
      {/* OptimizationPanel: sempre montato, visibile solo quando il tab è attivo.
          display:none preserva tutto lo stato React (autoFixEnabled, coda, segnali). */}
      <div style={{
        display: optimizationVisible ? "flex" : "none",
        flexDirection: "column",
        height: "100%",
        minHeight: 0,
      }}>
        {!project ? (
          <div style={{ padding: 12, color: tc.textMuted }}>Apri un progetto per usare il pannello.</div>
        ) : (
          <OptimizationPanel
            projectId={project.id}
            onSendToChat={onSendToChat}
            onAutoSendToChat={onAutoSendToChat}
            agentRunEndSignal={agentRunEndSignal}
          />
        )}
      </div>
      {/* Tutti gli altri pannelli: montati solo quando attivi */}
      {!optimizationVisible && (
        <div style={{ display: "flex", flexDirection: "column", height: "100%", minHeight: 0, flex: 1, minWidth: 0 }}>
          {otherPanel()}
        </div>
      )}
    </>
  );
}
