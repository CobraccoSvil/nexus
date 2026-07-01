"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import {
  ackTerminalCommand,
  createTerminalSession,
  finishTerminalCommand,
  getApiBaseUrl,
  setTerminalPresence,
} from "../lib/api-client";
import { useTheme, useThemeColors } from "../lib/theme";

interface TerminalTab {
  id: string;
  label: string;
}

// Derive the WebSocket base from the page origin so it works through HTTPS proxies.
// Lato browser usa /neural: il custom server (server.js) riscrive /neural/* in
// /api/neural/* e inoltra a mcp-core, dove il WS del terminale e' ora esposto
// (il brain Python e' stato eliminato). Il path finale risulta
// host/neural/ws/terminal/{id} -> proxy -> mcp-core/api/neural/ws/terminal/{id}.
// Fallback env solo per SSR/dev (non usato dal browser).
function getNeuralWsBase(): string {
  if (typeof window !== "undefined") {
    const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
    return `${proto}//${window.location.host}/neural`;
  }
  return (process.env.NEXT_PUBLIC_NEURAL_URL || "http://localhost:4000").replace(/^http/, "ws");
}
const NEURAL_WS = getNeuralWsBase();

let idCounter = 0;
function genId() {
  return `term-${Date.now()}-${++idCounter}`;
}

function TerminalInstance({
  tabId,
  active,
  isDark,
  projectId,
  onReady,
  onOutput,
}: {
  tabId: string;
  active: boolean;
  isDark: boolean;
  projectId?: string;
  onReady?: (write: (data: string) => boolean) => void;
  onOutput?: (data: string) => void;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const stateRef = useRef<{ cleanup?: () => void; fitAddon?: { fit: () => void } }>({});
  const onReadyRef = useRef(onReady);
  const onOutputRef = useRef(onOutput);

  useEffect(() => {
    onReadyRef.current = onReady;
  }, [onReady]);

  useEffect(() => {
    onOutputRef.current = onOutput;
  }, [onOutput]);

  useEffect(() => {
    if (!containerRef.current) return;
    let disposed = false;

    (async () => {
      const { Terminal } = await import("@xterm/xterm");
      const { FitAddon } = await import("@xterm/addon-fit");
      const { WebLinksAddon } = await import("@xterm/addon-web-links");

      if (disposed) return;

      // xterm misura i glifi su canvas e NON risolve le CSS var: leggiamo il
      // valore computed del token unico (--font-mono, vedi globals.css) e lo
      // passiamo risolto, cosi' il punto unico tipografico (regola L) resta uno.
      const fontMono =
        getComputedStyle(document.documentElement)
          .getPropertyValue("--font-mono")
          .trim() || "monospace";
      const term = new Terminal({
        cursorBlink: true,
        fontSize: 13,
        fontFamily: fontMono,
        theme: isDark
          ? { background: "#0d1117", foreground: "#e6edf3", cursor: "#58a6ff", selectionBackground: "#264f78" }
          : { background: "#f5f7fa", foreground: "#1a2332", cursor: "#2b6cb0", selectionBackground: "#d0e2f7" },
      });
      const fitAddon = new FitAddon();
      term.loadAddon(fitAddon);
      term.loadAddon(new WebLinksAddon());
      term.open(containerRef.current!);
      fitAddon.fit();

      if (!projectId) {
        term.writeln("\x1b[90mSeleziona un progetto per aprire il terminale.\x1b[0m");
        stateRef.current = {
          fitAddon,
          cleanup: () => term.dispose(),
        };
        return;
      }

      let ws: WebSocket | null = null;
      let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
      let handlePaste: ((event: ClipboardEvent) => void) | null = null;
      let disposedLocal = false;
      let retryCount = 0;
      const MAX_RETRIES = 6;

      const exposeDisconnectedWriter = () => {
        onReadyRef.current?.(() => false);
      };

      const scheduleReconnect = (reason?: "abnormal" | "crash") => {
        if (disposedLocal || reconnectTimer) return;
        if (retryCount >= MAX_RETRIES) {
          term.writeln(
            `\x1b[31m[Terminale non raggiungibile dopo ${MAX_RETRIES} tentativi. ` +
            `Riapri la scheda per riprovare.]\x1b[0m`,
          );
          return;
        }
        // Exponential backoff: 1.2s, 2.4s, 4.8s, 9.6s, 19.2s, 30s
        const delay = Math.min(30000, 1200 * Math.pow(2, retryCount));
        retryCount++;
        reconnectTimer = setTimeout(() => {
          reconnectTimer = null;
          if (!disposedLocal) {
            const label = reason === "crash"
              ? "\x1b[90m[Riapertura terminale...]\x1b[0m"
              : `\x1b[90m[Riconnessione (${retryCount}/${MAX_RETRIES})...]\x1b[0m`;
            term.writeln(label);
            void connectTerminal();
          }
        }, delay);
      };

      const connectTerminal = async () => {
        try {
          const session = await createTerminalSession(projectId);
          if (disposed || disposedLocal) {
            return;
          }

          ws = new WebSocket(
            `${NEURAL_WS}/ws/terminal/${session.sessionId}?token=${encodeURIComponent(session.token)}`,
          );
          ws.binaryType = "arraybuffer";

          ws.onopen = () => {
            retryCount = 0; // reset backoff on successful connection
            term.writeln(`\x1b[90m[Terminale connesso: ${session.workingDirectory}]\x1b[0m`);
            ws?.send(JSON.stringify({ type: "resize", rows: term.rows, cols: term.cols }));
            onReadyRef.current?.((data: string) => {
              if (ws?.readyState === WebSocket.OPEN) {
                ws.send(data);
                return true;
              }
              return false;
            });
          };

          ws.onmessage = (event) => {
            if (event.data instanceof ArrayBuffer) {
              const bytes = new Uint8Array(event.data);
              term.write(bytes);
              onOutputRef.current?.(new TextDecoder().decode(bytes));
            } else if (typeof event.data === "string") {
              // Filter control messages (process_exit JSON) — don't print them raw
              if (event.data.startsWith('{"type":"process_exit"')) {
                try {
                  const msg = JSON.parse(event.data) as { type: string; exitCode: number };
                  if (msg.type === "process_exit") {
                    const code = msg.exitCode;
                    const isSignal = code < 0;
                    const label = isSignal
                      ? `\x1b[31m[Processo terminato dal segnale ${-code}]\x1b[0m`
                      : code === 0
                        ? `\x1b[90m[Processo terminato (exit 0)]\x1b[0m`
                        : `\x1b[33m[Processo terminato (exit ${code})]\x1b[0m`;
                    term.writeln(`\r\n${label}`);
                    onOutputRef.current?.(event.data);
                    // Auto-restart shell after a brief delay if it crashed (signal exit)
                    if (isSignal && !disposed && !disposedLocal) {
                      setTimeout(() => {
                        if (!disposed && !disposedLocal) {
                          // Close with code 4000 (abnormal/crash) to trigger reconnect
                          ws?.close(4000, "process_signal_exit");
                        }
                      }, 1500);
                    }
                    return;
                  }
                } catch { /* fall through */ }
              }
              term.write(event.data);
              onOutputRef.current?.(event.data);
            }
          };

          ws.onclose = (event) => {
            exposeDisconnectedWriter();
            if (disposed || disposedLocal) return;
            // Don't reconnect on intentional/clean close (1000 = normal, 1001 = going away)
            if (event.code === 1000 || event.code === 1001) return;
            // Rifiuto deterministico del server (4400-4499, es. 4403 "sessione
            // non valida"): errore non transitorio. Riconnettere genererebbe un
            // loop infinito di apertura/chiusura. Ci si ferma con un messaggio
            // chiaro e niente backoff.
            if (event.code >= 4400 && event.code <= 4499) {
              term.writeln(
                "\r\n\x1b[31m[Terminale non disponibile per questo progetto: " +
                "sessione rifiutata dal server. Verifica che il progetto sia " +
                "registrato correttamente.]\x1b[0m",
              );
              return;
            }
            const isCrash = event.code === 4000;
            term.write("\r\n\x1b[90m[Connessione terminale chiusa]\x1b[0m\r\n");
            scheduleReconnect(isCrash ? "crash" : "abnormal");
          };

          ws.onerror = () => {
            // La chiusura scatena onclose; lasciamo la riconnessione li'.
          };
        } catch (error) {
          if (disposed || disposedLocal) return;
          // Ignore AbortError — triggered by component unmount or navigation, not a real error
          if (error instanceof Error && error.name === "AbortError") return;
          term.writeln(
            `\x1b[31mImpossibile avviare il terminale: ${
              error instanceof Error ? error.message : "errore sconosciuto"
            }\x1b[0m`,
          );
          exposeDisconnectedWriter();
          scheduleReconnect("abnormal");
        }
      };

      try {
        exposeDisconnectedWriter();
        await connectTerminal();

        const pasteFromClipboard = async () => {
          try {
            const text = await navigator.clipboard.readText();
            if (!text) return;
            if (ws?.readyState === WebSocket.OPEN) {
              ws.send(text);
            } else {
              term.paste(text);
            }
          } catch {
            // ignore clipboard permission errors
          }
        };

        term.attachCustomKeyEventHandler((event: KeyboardEvent) => {
          if (event.type !== "keydown") return true;
          const key = event.key.toLowerCase();
          const isMac = navigator.platform.toUpperCase().includes("MAC");
          const modifierPressed = isMac ? event.metaKey : event.ctrlKey;

          if ((modifierPressed && key === "v") || (event.shiftKey && key === "insert")) {
            event.preventDefault();
            void pasteFromClipboard();
            return false;
          }

          if ((modifierPressed && key === "c") || (modifierPressed && event.shiftKey && key === "c")) {
            if (term.hasSelection()) {
              const selected = term.getSelection();
              if (selected) {
                void navigator.clipboard.writeText(selected).catch(() => {});
              }
              event.preventDefault();
              return false;
            }
            return true;
          }

          if (modifierPressed && key === "x") {
            if (term.hasSelection()) {
              const selected = term.getSelection();
              if (selected) {
                void navigator.clipboard.writeText(selected).catch(() => {});
              }
              event.preventDefault();
              return false;
            }
            return true;
          }

          return true;
        });

        handlePaste = (event: ClipboardEvent) => {
          const text = event.clipboardData?.getData("text");
          if (!text) return;
          event.preventDefault();
          if (ws?.readyState === WebSocket.OPEN) {
            ws.send(text);
          } else {
            term.paste(text);
          }
        };
        containerRef.current?.addEventListener("paste", handlePaste);

        term.onData((data: string) => {
          if (ws?.readyState === WebSocket.OPEN) {
            ws.send(data);
          }
        });

        term.onResize(({ rows, cols }: { rows: number; cols: number }) => {
          if (ws?.readyState === WebSocket.OPEN) {
            ws.send(JSON.stringify({ type: "resize", rows, cols }));
          }
        });
      } catch (error) {
        term.writeln(
          `\x1b[31mImpossibile avviare il terminale: ${
            error instanceof Error ? error.message : "errore sconosciuto"
          }\x1b[0m`,
        );
      }

      const ro = new ResizeObserver(() => {
        try {
          fitAddon.fit();
        } catch {}
      });
      ro.observe(containerRef.current!);

      stateRef.current = {
        fitAddon,
        cleanup: () => {
          disposedLocal = true;
          if (reconnectTimer) {
            clearTimeout(reconnectTimer);
            reconnectTimer = null;
          }
          exposeDisconnectedWriter();
          ro.disconnect();
          if (handlePaste) {
            containerRef.current?.removeEventListener("paste", handlePaste);
          }
          ws?.close();
          term.dispose();
        },
      };
    })();

    return () => {
      disposed = true;
      stateRef.current.cleanup?.();
    };
  }, [isDark, projectId, tabId]);

  useEffect(() => {
    if (active) {
      setTimeout(() => stateRef.current.fitAddon?.fit(), 50);
    }
  }, [active]);

  return (
    <div
      ref={containerRef}
      style={{
        width: "100%",
        height: "100%",
        display: active ? "block" : "none",
      }}
    />
  );
}

export function TerminalPanel({
  projectId,
  projectLabel,
  embedded = false,
  onActivate,
  onOutput,
}: {
  projectId?: string;
  projectLabel?: string;
  embedded?: boolean;
  onActivate?: () => void;
  // Inoltra ogni chunk grezzo della shell (con ANSI) a un consumer esterno,
  // es. il BottomPanelManager che alimenta il pannello Debug.
  onOutput?: (chunk: string) => void;
}) {
  const tc = useThemeColors();
  const { resolved } = useTheme();
  const isDark = resolved === "dark";
  const [tabs, setTabs] = useState<TerminalTab[]>([{ id: genId(), label: "shell" }]);
  const [activeId, setActiveId] = useState(() => tabs[0]?.id);
  // Map tabId → writer function per command injection (true se inviato al backend terminale)
  const writeRefs = useRef<Record<string, (data: string) => boolean>>({});
  const outputRef = useRef<string>("");
  const onOutputRef = useRef(onOutput);
  onOutputRef.current = onOutput;
  const consumerIdRef = useRef<string>(`terminal-${Date.now()}-${Math.random().toString(16).slice(2)}`);

  const projectIdRef = useRef<string | undefined>(projectId);

  useEffect(() => {
    if (!projectId) return;
    if (projectIdRef.current === projectId) return;
    projectIdRef.current = projectId;
    const fresh = { id: genId(), label: "shell" };
    setTabs([fresh]);
    setActiveId(fresh.id);
    writeRefs.current = {};
  }, [projectId]);

  // Subscribe to agent terminal commands via SSE
  useEffect(() => {
    if (!projectId) return;
    const base = getApiBaseUrl();
    const consumerId = consumerIdRef.current;
    const url = `${base}/api/projects/${projectId}/terminal-commands/stream?consumerId=${encodeURIComponent(consumerId)}`;
    const es = new EventSource(url, { withCredentials: true });
    void setTerminalPresence(projectId, consumerId, true).catch(() => {});

    // Heartbeat: ri-registra presence ogni 30s per sopravvivere a restart del backend
    const heartbeat = window.setInterval(() => {
      void setTerminalPresence(projectId, consumerId, true).catch(() => {});
    }, 30000);

    es.addEventListener("terminal_command", (e: MessageEvent) => {
      try {
        const raw = JSON.parse(e.data) as { command: string; commandId: string };
        const commandId = raw.commandId;
        const wantNewTab = raw.command.startsWith("[NEW_TAB]");
        const command = wantNewTab ? raw.command.slice(9) : raw.command;

        onActivate?.(); // porta in primo piano il pannello terminale se nascosto

        const sendToTerminal = (writeFn: (data: string) => boolean) => {
          const before = outputRef.current.length;
          const sent = writeFn(command + "\r");
          if (sent) {
            // ACK immediato: comando ricevuto
            void ackTerminalCommand(projectId, commandId, {
              consumerId,
              delivered: true,
              outputPreview: "",
            }).catch(() => {});

            // Strip ANSI escape sequences per output leggibile
            const stripAnsi = (s: string) =>
              s.replace(/\x1B\[[0-9;]*[A-Za-z]/g, "")
               .replace(/\x1B\][^\x07]*\x07/g, "")
               .replace(/\x1B\([A-Z]/g, "")
               .replace(/[\x1B\r]/g, "");

            // Debounce: poll ogni 1s, dopo 3s stabili manda finish
            let finished = false;
            let lastLen = before;
            let stableCount = 0;

            const sendFinish = () => {
              if (finished) return;
              finished = true;
              const rawOutput = outputRef.current.slice(before);
              const cleanOutput = stripAnsi(rawOutput).trim();
              // Cerca exit code nel buffer (process_exit JSON dal Python WebSocket)
              let exitCode: number | null = null;
              const exitMatch = rawOutput.match(/\{"type":"process_exit","exitCode":(-?\d+)\}/);
              if (exitMatch) exitCode = parseInt(exitMatch[1], 10);
              void finishTerminalCommand(projectId, commandId, {
                consumerId,
                exitCode,
                fullOutput: cleanOutput.slice(-4000),
              }).catch(() => {});
            };

            // Poll output ogni 1s: se stabile per 3 cicli, manda finish
            const pollInterval = window.setInterval(() => {
              if (finished) { clearInterval(pollInterval); return; }
              const currentLen = outputRef.current.length;
              if (currentLen === lastLen) {
                stableCount++;
                if (stableCount >= 3) {
                  clearInterval(pollInterval);
                  sendFinish();
                }
              } else {
                lastLen = currentLen;
                stableCount = 0;
              }
            }, 1000);

            // Timeout massimo 120s
            window.setTimeout(() => {
              if (!finished) { clearInterval(pollInterval); sendFinish(); }
            }, 120000);
          } else {
            void ackTerminalCommand(projectId, commandId, {
              consumerId,
              delivered: false,
              error: "Terminale non connesso: comando non inviato",
            }).catch(() => {});
          }
        };

        const writeFn = wantNewTab ? null : (writeRefs.current[activeId ?? ""] ?? Object.values(writeRefs.current)[0]);
        if (writeFn) {
          // Terminale già pronto: invia subito
          sendToTerminal(writeFn);
        } else {
          // Nessun terminale aperto: crea un nuovo tab e attendi il mount
          const newTab = { id: genId(), label: wantNewTab ? "service" : "shell" };
          setTabs((prev) => [...prev, newTab]);
          setActiveId(newTab.id);

          // Poll ogni 150ms finché il terminale non si monta (max 5s)
          let attempts = 0;
          const interval = window.setInterval(() => {
            attempts++;
            const fn = writeRefs.current[newTab.id] ?? Object.values(writeRefs.current)[0];
            if (fn) {
              clearInterval(interval);
              sendToTerminal(fn);
            } else if (attempts > 33) {
              clearInterval(interval);
              void ackTerminalCommand(projectId, commandId, {
                consumerId,
                delivered: false,
                error: "Terminale non pronto dopo 5 secondi",
              }).catch(() => {});
            }
          }, 150);
        }
      } catch {
        // ignore parse errors
      }
    });

    es.onerror = () => {
      // EventSource auto-reconnects; no action needed
    };

    return () => {
      clearInterval(heartbeat);
      es.close();
      void setTerminalPresence(projectId, consumerId, false).catch(() => {});
    };
  // dipendenze escluse intenzionalmente: activeId e onActivate cambiano ad ogni switch di tab; includere causerebbe restart SSE ad ogni cambio tab
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId]);

  const addTab = useCallback(() => {
    const newTab = { id: genId(), label: "shell" };
    setTabs((prev) => [...prev, newTab]);
    setActiveId(newTab.id);
  }, []);

  const closeTab = useCallback((id: string) => {
    setTabs((prev) => {
      const next = prev.filter((tab) => tab.id !== id);
      if (next.length === 0) {
        const fresh = { id: genId(), label: "shell" };
        setActiveId(fresh.id);
        return [fresh];
      }
      if (id === activeId) {
        setActiveId(next[0].id);
      }
      return next;
    });
  }, [activeId]);

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        height: "100%",
        borderRadius: embedded ? 0 : 12,
        overflow: "hidden",
        border: embedded ? "none" : `1px solid ${tc.border}`,
        background: isDark ? "#0d1117" : "#f5f7fa",
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 2,
          padding: "4px 8px",
          background: tc.bgCard,
          borderBottom: `1px solid ${tc.border}`,
          fontSize: 12,
          flexShrink: 0,
        }}
      >
        {tabs.map((tab, index) => (
          <div
            key={tab.id}
            onClick={() => setActiveId(tab.id)}
            style={{
              padding: "4px 10px",
              borderRadius: 6,
              cursor: "pointer",
              background: tab.id === activeId ? tc.accentBg : "transparent",
              color: tab.id === activeId ? tc.accent : tc.textMuted,
              display: "flex",
              alignItems: "center",
              gap: 6,
              fontWeight: tab.id === activeId ? 600 : 400,
            }}
          >
            {tab.label} {index + 1}
            <span
              onClick={(event) => {
                event.stopPropagation();
                closeTab(tab.id);
              }}
              style={{ cursor: "pointer", opacity: 0.5, fontSize: 10 }}
            >
              ×
            </span>
          </div>
        ))}
        <button
          onClick={addTab}
          title="Nuovo terminale"
          style={{
            background: "none",
            border: "none",
            color: tc.textMuted,
            cursor: "pointer",
            fontSize: 16,
            padding: "2px 6px",
          }}
        >
          +
        </button>
        <div style={{ marginLeft: "auto", color: tc.textMuted, fontSize: 11 }}>
          {projectLabel ? `Progetto: ${projectLabel}` : "Nessun progetto attivo"}
        </div>
      </div>
      <div style={{ flex: 1, position: "relative", minHeight: 0 }}>
        {tabs.map((tab) => (
          <TerminalInstance
            key={tab.id}
            tabId={tab.id}
            active={tab.id === activeId}
            isDark={isDark}
            projectId={projectId}
            onReady={(writeFn) => { writeRefs.current[tab.id] = writeFn; }}
            onOutput={(chunk) => {
              if (!chunk) return;
              const next = `${outputRef.current}${chunk}`;
              outputRef.current = next.length > 8000 ? next.slice(-8000) : next;
              // Inoltra al consumer esterno (es. pannello Debug)
              onOutputRef.current?.(chunk);
            }}
          />
        ))}
      </div>
    </div>
  );
}
