"use client";

import { useThemeColors } from "../../lib/theme";
import { TerminalPanel } from "../terminal-panel";
import { DebugPanel } from "./debug-panel";
import { OutputPanel } from "./output-panel";
import { OptimizationPanel } from "./optimization-panel";
import { MonitorPanel } from "./monitor-panel";
import { RunPanel } from "./run-panel";
import { SecurityPanel } from "./security-panel";
import { promptFromPlaywrightRun, promptFromProblem, promptEnablePlaywright, promptRunPlaywrightTests } from "../../lib/chat-prompts";
import type {
  AITraceEvent,
  OutputChannel,
  OutputEvent,
  PlaywrightArtifact,
  PlaywrightProgress,
  PlaywrightRunSummary,
  PortEntry,
  ProblemItem,
  UserProjectDetails,
} from "../../lib/api-client";
import { subscribePlaywrightRunStream } from "../../lib/api-client";
import { useState, useEffect, useRef } from "react";
import { useGlobalDialog } from "../global-dialog-provider";

export type PanelTab =
  | "problems"
  | "output"
  | "debug"
  | "run"
  | "terminal"
  | "services"
  | "ports"
  | "playwright"
  | "optimization"
  | "monitor"
  | "security";

export interface BottomPanelManagerProps {
  activePanelTab: PanelTab;
  project: UserProjectDetails | null;
  problemItems: ProblemItem[];
  outputChannels: OutputChannel[];
  selectedOutputChannel: string;
  outputEvents: OutputEvent[];
  ports: PortEntry[];
  playwrightRuns: PlaywrightRunSummary[];
  playwrightConfigured?: boolean;
  traces?: AITraceEvent[];
  onOpenFile: (path: string, line?: number) => void;
  onSelectOutputChannel: (id: string) => void;
  onClearPanel?: (tab: PanelTab) => void;
  onRefreshPanel?: (tab: PanelTab) => void;
  onSendToChat?: (message: string) => void;
  onAutoSendToChat?: (message: string) => void;
  onKillPort?: (port: number) => void | Promise<void>;
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
  playwrightConfigured,
  onOpenFile,
  onSelectOutputChannel,
  onClearPanel,
  onRefreshPanel,
  onSendToChat,
  onAutoSendToChat,
  onKillPort,
  agentRunEndSignal,
}: BottomPanelManagerProps) {
  const tc = useThemeColors();
  const { confirmDialog } = useGlobalDialog();

  const clearBar = (tab: PanelTab, hasContent: boolean) =>
    (hasContent || onRefreshPanel) ? (
      <div style={{ display: "flex", justifyContent: "flex-end", padding: "4px 8px", borderBottom: `1px solid ${tc.border}`, flexShrink: 0 }}>
        {onRefreshPanel && (
          <button
            onClick={() => onRefreshPanel(tab)}
            title="Ricarica contenuto"
            style={{
              background: "none",
              border: `1px solid ${tc.border}`,
              borderRadius: 4,
              color: tc.textMuted,
              cursor: "pointer",
              padding: "2px 8px",
              fontSize: 11,
              marginRight: 6,
            }}
          >
            Refresh
          </button>
        )}
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
          disabled={!hasContent}
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

    if (activePanelTab === "monitor") return <MonitorPanel onSendToChat={onSendToChat} />;

    if (activePanelTab === "problems") return (
      <div style={{ display: "flex", flexDirection: "column", height: "100%", minHeight: 0 }}>
        {clearBar("problems", problemItems.length > 0)}
        <div style={{ display: "flex", flexDirection: "column", gap: 8, padding: 10, minHeight: 0, overflow: "auto", flex: 1 }}>
          {problemItems.length === 0 ? (
            <div style={{ color: tc.textMuted }}>Nessun problema aperto.</div>
          ) : (
            problemItems.map((item) => (
              <div
                key={item.id}
                style={{
                  display: "flex",
                  alignItems: "stretch",
                  gap: 8,
                }}
              >
                <button
                  onClick={() => item.filePath && onOpenFile(item.filePath, item.line ?? 1)}
                  style={{ ...listRowButton(tc), flex: 1, minWidth: 0 }}
                >
                  <div style={{ display: "flex", justifyContent: "space-between", columnGap: 10 }}>
                    <span style={{ color: tc.text, fontSize: 12, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                      {item.message}
                    </span>
                    <span style={{ color: severityColor(item.severity, tc), fontSize: 11, flexShrink: 0 }}>
                      {item.severity}
                    </span>
                  </div>
                  <div style={{ color: tc.textMuted, fontSize: 11, marginTop: 4 }}>
                    {item.source}{item.filePath ? ` • ${item.filePath}${item.line ? `:${item.line}` : ""}` : ""}
                  </div>
                </button>
                {onSendToChat && (
                  <button
                    type="button"
                    onClick={() => {
                      onSendToChat(promptFromProblem(item));
                    }}
                    title="Invia in chat un prompt per risolvere"
                    style={{
                      flexShrink: 0,
                      marginLeft: 2,
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
                    }}
                    aria-label="Chiedi a Nexus"
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

    if (activePanelTab === "debug") return <DebugPanel projectId={project.id} onSendToChat={onSendToChat} />;

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
                  <span style={{ color: tc.text, fontWeight: 500, flex: 1, minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                    {port.label || `Porta ${port.port}`}
                  </span>
                  {onKillPort && typeof port.port === "number" && (() => {
                    // Narrowing manuale: la callback gira async, TS non
                    // propaga il type guard del rendering condizionale.
                    const portNum: number = port.port;
                    return (
                    <button
                      type="button"
                      onClick={async () => {
                        const ok = await confirmDialog(
                          `Terminare il processo in ascolto sulla porta ${portNum} e rilasciare l'allocazione?`,
                          "Termina porta",
                        );
                        if (ok) {
                          void onKillPort(portNum);
                        }
                      }}
                      title="Termina processo e rilascia porta"
                      style={{
                        background: "rgba(239,68,68,0.85)",
                        color: "#fff",
                        border: "none",
                        borderRadius: 3,
                        padding: "0 8px",
                        fontSize: 11,
                        cursor: "pointer",
                        verticalAlign: "middle",
                        lineHeight: "18px",
                        height: 18,
                        fontWeight: 600,
                        flexShrink: 0,
                      }}
                    >
                      🗑 kill
                    </button>
                    );
                  })()}
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

    if (activePanelTab === "security") return (
      <SecurityPanel projectId={project.id} onSendToChat={onSendToChat} />
    );

    // playwright
    const handleRunPlaywright = () => {
      if (!onSendToChat) return;
      // Fix M43: pulisce la lista delle run precedenti prima di lanciare i
      // nuovi test. Senza questo, le run vecchie restavano visibili sotto
      // le nuove confondendo l'utente sullo stato attuale.
      onClearPanel?.("playwright");
      onSendToChat(promptRunPlaywrightTests());
    };
    const handleEnablePlaywright = () => {
      if (!onSendToChat) return;
      onSendToChat(promptEnablePlaywright(ports));
    };
    return (
      <div style={{ display: "flex", flexDirection: "column", height: "100%", minHeight: 0 }}>
        <div style={{
          display: "flex", alignItems: "center", justifyContent: "space-between",
          padding: "4px 10px", borderBottom: `1px solid ${tc.border}`, flexShrink: 0,
          gap: 8,
        }}>
          <span style={{ fontSize: 11, color: tc.textMuted }}>
            {playwrightRuns.length > 0
              ? `${playwrightRuns.length} run`
              : playwrightConfigured ? "Configurato" : "Nessun run"}
          </span>
          <div style={{ display: "flex", gap: 6 }}>
            {onSendToChat && !playwrightConfigured && (
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
              {playwrightConfigured ? (
                <>Playwright configurato. Premi <strong>Avvia test</strong> per eseguire i test e2e.</>
              ) : (
                <>Nessun run Playwright disponibile. Premi <strong>Abilita Playwright</strong> per configurare il framework di test, poi <strong>Avvia test</strong> per eseguirli.</>
              )}
            </div>
          ) : (
            playwrightRuns.map((run) => {
              // Fix M46: distingue setup (install deps/browsers) da test execution.
              // I jobs in DB hanno tutti kind='playwright_test' ma alcuni sono
              // chiaramente install (apt-get, pnpm add @playwright/test, playwright
              // install chromium). Classificazione regex sul label+summary.
              const s = `${run.label} ${run.summary ?? ""}`.toLowerCase();
              const isSetup =
                s.includes("installing dependencies") ||
                s.includes("switching to root") ||
                s.includes("failed to install browsers") ||
                s.includes("@playwright/test") ||
                s.includes("chromium_headless_shell") ||
                /\b(ffmpeg|firefox-\d|webkit-\d)\b/.test(s) ||
                (s.includes("installed") && !s.includes("test"));
              const category: "setup" | "test" = isSetup ? "setup" : "test";
              const badgeBg = category === "setup" ? "#6b7280" : "#3b82f6";
              return (
              <div key={run.id} style={tileStyle(tc)}>
                <div style={{ display: "flex", justifyContent: "space-between", gap: 10, alignItems: "center" }}>
                  <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                    <span style={{
                      background: badgeBg, color: "#fff", fontSize: 9, fontWeight: 700,
                      padding: "1px 6px", borderRadius: 3, textTransform: "uppercase",
                      letterSpacing: 0.5,
                    }}>{category}</span>
                    <span className="font-semibold" style={{ color: tc.text }}>{run.label}</span>
                  </div>
                  <span style={{
                    color: run.status === "passed" ? "#10b981" : run.status === "failed" ? tc.error : tc.textMuted,
                    fontSize: 11, fontWeight: 600,
                  }}>{run.status}</span>
                </div>
                {run.summary && <div style={{ color: tc.textSecondary, fontSize: 12, marginTop: 6 }}>{run.summary}</div>}
                {project && category === "test" && (
                  <PlaywrightLiveProgress run={run} projectId={project.id} tc={tc} />
                )}
                {run.artifacts && run.artifacts.length > 0 && project && (
                  <PlaywrightArtifacts artifacts={run.artifacts} projectId={project.id} tc={tc} />
                )}
                <div style={{ color: tc.textMuted, fontSize: 11, marginTop: 6 }}>{new Date(run.createdAt).toLocaleString()}</div>
                {onSendToChat && run.status === "failed" && (
                  <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 8 }}>
                    <button
                      onClick={() => {
                        onSendToChat(promptFromPlaywrightRun(run));
                      }}
                      title="Invia questo run fallito alla chat di Nexus"
                      style={{
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
                      }}
                    >
                      ↗ chat
                    </button>
                  </div>
                )}
              </div>
              );
            })
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

/**
 * Componente live: si aggancia all'SSE stream del run per mostrare progress
 * incrementale (counter passed/failed/skipped, spec corrente) + ultime ~30
 * righe di output. Si disconnette automaticamente sul Final.
 */
function PlaywrightLiveProgress({
  run,
  projectId,
  tc,
}: {
  run: PlaywrightRunSummary;
  projectId: string;
  tc: ReturnType<typeof useThemeColors>;
}) {
  const [progress, setProgress] = useState<PlaywrightProgress>(
    (run.progress as PlaywrightProgress) || {
      passed: 0, failed: 0, skipped: 0, flaky: 0,
    }
  );
  const [tail, setTail] = useState<string[]>([]);
  const esRef = useRef<EventSource | null>(null);

  useEffect(() => {
    if (run.status !== "running") return;
    const es = subscribePlaywrightRunStream(projectId, run.id, {
      onProgress: (data) => setProgress(data.progress),
      onLine: (data) => {
        setTail((prev) => {
          const next = [...prev, data.line];
          return next.length > 30 ? next.slice(-30) : next;
        });
      },
      onFinal: (data) => {
        setProgress(data.progress);
        es.close();
      },
      onError: () => { /* il browser auto-reconnect; ignora */ },
    });
    esRef.current = es;
    return () => { es.close(); esRef.current = null; };
  }, [run.id, run.status, projectId]);

  const total = progress.total ?? (progress.passed + progress.failed + progress.skipped);
  const done = progress.passed + progress.failed + progress.skipped;
  const pct = total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0;
  const isRunning = run.status === "running";

  return (
    <div style={{ marginTop: 8, fontSize: 11 }}>
      {/* Counter compatti */}
      <div style={{ display: "flex", gap: 12, alignItems: "center", color: tc.textSecondary }}>
        <span>
          <span style={{ color: "#10b981", fontWeight: 600 }}>{progress.passed} ✓</span>
          {progress.failed > 0 && (
            <> · <span style={{ color: tc.error, fontWeight: 600 }}>{progress.failed} ✗</span></>
          )}
          {progress.skipped > 0 && (
            <> · <span style={{ color: tc.textMuted }}>{progress.skipped} skip</span></>
          )}
          {progress.flaky > 0 && (
            <> · <span style={{ color: "#f59e0b" }}>{progress.flaky} flaky</span></>
          )}
        </span>
        <span style={{ color: tc.textMuted }}>
          {done}{total > 0 ? `/${total}` : ""} {isRunning ? "in corso" : ""}
        </span>
      </div>

      {/* Barra di progress (solo per run live o con progress popolato) */}
      {(isRunning || total > 0) && (
        <div style={{
          marginTop: 4, height: 4, background: tc.border, borderRadius: 2,
          overflow: "hidden", position: "relative",
        }}>
          <div style={{
            width: `${pct}%`, height: "100%",
            background: progress.failed > 0 ? tc.error : "#10b981",
            transition: "width 0.3s ease",
          }} />
        </div>
      )}

      {/* Spec attualmente in esecuzione */}
      {isRunning && progress.current_spec && (
        <div style={{ marginTop: 4, color: tc.textMuted, fontFamily: "monospace", fontSize: 10 }}>
          ▸ {progress.current_spec}
        </div>
      )}

      {/* Tail output live (solo per run in corso) */}
      {isRunning && tail.length > 0 && (
        <div style={{
          marginTop: 6, padding: 6, background: "rgba(0,0,0,0.2)",
          borderRadius: 3, fontFamily: "monospace", fontSize: 10, lineHeight: 1.4,
          color: tc.textMuted, maxHeight: 120, overflow: "auto",
          whiteSpace: "pre-wrap",
        }}>
          {tail.map((line, i) => (
            <div key={i} style={{
              color: line.includes("✗") || line.includes("Error") ? tc.error
                : line.includes("✓") ? "#10b981"
                : undefined,
            }}>{line}</div>
          ))}
        </div>
      )}

      {/* Lista specifica dei failed */}
      {!isRunning && progress.failed_specs && progress.failed_specs.length > 0 && (
        <div style={{ marginTop: 4, color: tc.error, fontFamily: "monospace", fontSize: 10 }}>
          {progress.failed_specs.slice(0, 5).map((s, i) => (
            <div key={i}>✗ {s}</div>
          ))}
          {progress.failed_specs.length > 5 && (
            <div style={{ color: tc.textMuted }}>
              ...e altri {progress.failed_specs.length - 5}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function PlaywrightArtifacts({
  artifacts,
  projectId,
  tc,
}: {
  artifacts: PlaywrightArtifact[];
  projectId: string;
  tc: ReturnType<typeof useThemeColors>;
}) {
  const [lightbox, setLightbox] = useState<string | null>(null);
  const images = artifacts.filter((a) => a.kind === "image");
  const videos = artifacts.filter((a) => a.kind === "video");
  const others = artifacts.filter((a) => a.kind !== "image" && a.kind !== "video");

  const urlFor = (path: string) =>
    `/api/projects/${projectId}/playwright/artifact?path=${encodeURIComponent(path)}`;

  return (
    <div style={{ marginTop: 8 }}>
      {images.length > 0 && (
        <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
          {images.slice(0, 12).map((img) => (
            <button
              key={img.path}
              onClick={() => setLightbox(urlFor(img.path))}
              title={img.name}
              style={{
                padding: 0,
                border: `1px solid ${tc.border}`,
                borderRadius: 4,
                cursor: "pointer",
                background: tc.bgCard,
                overflow: "hidden",
              }}
            >
              <img
                src={urlFor(img.path)}
                alt={img.name}
                loading="lazy"
                style={{ width: 120, height: 80, objectFit: "cover", display: "block" }}
              />
            </button>
          ))}
          {images.length > 12 && (
            <span style={{ alignSelf: "center", fontSize: 11, color: tc.textMuted }}>
              +{images.length - 12} altre immagini
            </span>
          )}
        </div>
      )}
      {videos.length > 0 && (
        <div style={{ display: "flex", flexWrap: "wrap", gap: 6, marginTop: 6 }}>
          {videos.map((v) => (
            <video
              key={v.path}
              src={urlFor(v.path)}
              controls
              style={{ width: 200, height: 120, border: `1px solid ${tc.border}`, borderRadius: 4 }}
            />
          ))}
        </div>
      )}
      {others.length > 0 && (
        <div style={{ display: "flex", flexWrap: "wrap", gap: 6, marginTop: 6 }}>
          {others.map((o) => (
            <a
              key={o.path}
              href={urlFor(o.path)}
              target="_blank"
              rel="noreferrer"
              style={{
                fontSize: 11,
                padding: "2px 6px",
                borderRadius: 3,
                background: tc.bgCard,
                color: tc.textSecondary,
                textDecoration: "none",
                border: `1px solid ${tc.border}`,
              }}
            >
              {o.kind === "trace" ? "trace.zip" : o.kind === "report" ? "HTML report" : o.name}
            </a>
          ))}
        </div>
      )}
      {lightbox && (
        <div
          onClick={() => setLightbox(null)}
          style={{
            position: "fixed",
            inset: 0,
            background: "rgba(0,0,0,0.85)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            zIndex: 9999,
            cursor: "zoom-out",
          }}
        >
          <img src={lightbox} alt="Screenshot" style={{ maxWidth: "92vw", maxHeight: "92vh", boxShadow: "0 0 40px rgba(0,0,0,0.5)" }} />
        </div>
      )}
    </div>
  );
}
