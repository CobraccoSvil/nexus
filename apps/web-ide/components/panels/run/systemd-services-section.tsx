"use client";

import type { useThemeColors } from "../../../lib/theme";
import type { ProjectServiceEntry, PortEntry } from "../../../lib/api-client";
import { stateColor, stateLabel, actBtnStyle, hdrStyle, buildDiagnosticPrompt, type ServiceAction } from "./shared";

interface SystemdServicesSectionProps {
  tc: ReturnType<typeof useThemeColors>;
  services: ProjectServiceEntry[];
  slug: string;
  // ADR 0022: il bus systemd utente e' giu' (servizi installati ma non elencabili).
  managerUnavailable?: boolean;
  managerHint?: string;
  ports: PortEntry[];
  serviceUrlCache: Record<string, string>;
  svcBusy: Record<string, boolean>;
  svcMsg: string;
  batchBusy: boolean;
  pendingCount: number;
  changedFiles: Array<{ path: string; mtime: number }>;
  diagSentFor: string | null;
  diagResult: "pending" | "resolved" | "failed" | null;
  onSendToChat?: (message: string) => void;
  fetchServices: () => void;
  handleRestartAll: () => void;
  handleCleanupPorts: () => void;
  runWizard: () => void;
  handleSvcAction: (svc: ProjectServiceEntry, action: ServiceAction) => void;
  handleUninstall: (svc: ProjectServiceEntry) => void;
  bumpLastRestart: () => void;
  setDiagSentFor: (v: string | null) => void;
  setDiagResult: (v: "pending" | "resolved" | "failed" | null) => void;
}

export function SystemdServicesSection({
  tc,
  services,
  slug,
  managerUnavailable,
  managerHint,
  ports,
  serviceUrlCache,
  svcBusy,
  svcMsg,
  batchBusy,
  pendingCount,
  changedFiles,
  diagSentFor,
  diagResult,
  onSendToChat,
  fetchServices,
  handleRestartAll,
  handleCleanupPorts,
  runWizard,
  handleSvcAction,
  handleUninstall,
  bumpLastRestart,
  setDiagSentFor,
  setDiagResult,
}: SystemdServicesSectionProps) {
  return (
    <>
      {/* ════════════════════════════════ A: SERVIZI SYSTEMD ══════════════ */}
      <div style={hdrStyle(tc)} title="Servizi systemd persistenti del progetto: vivono in ~/.config/systemd/user/, partono al boot se enabled, vengono riavviati dal sistema se crashano.">
        <span>Servizi systemd persistenti{slug ? ` — ${slug}` : ""}</span>
        <div style={{ display:"flex", gap:6 }}>
          <button onClick={fetchServices} title="Aggiorna stato" disabled={batchBusy} style={{ background:"none",border:`1px solid ${tc.border}`,borderRadius:3,color:tc.textMuted,cursor:batchBusy?"wait":"pointer",padding:"1px 8px",fontSize:10 }}>↺</button>
          <button onClick={handleRestartAll} title={managerUnavailable ? "Manager systemd utente non attivo — avvialo prima di gestire i servizi" : services.length===0 ? "Nessun servizio systemd. Clicca per dettagli." : "Riavvia tutti i servizi del progetto"} disabled={batchBusy} style={{ background:"transparent",border:`1px solid #f59e0b`,borderRadius:3,color:"#f59e0b",cursor:batchBusy?"wait":"pointer",padding:"1px 8px",fontSize:10,opacity:batchBusy?0.5:1 }}>↻ Tutti</button>
          <button onClick={handleCleanupPorts} title="Termina processi su porte conflittuali (esclude i servizi del progetto)" disabled={batchBusy} style={{ background:"transparent",border:`1px solid #ef4444`,borderRadius:3,color:"#ef4444",cursor:batchBusy?"wait":"pointer",padding:"1px 8px",fontSize:10,opacity:batchBusy?0.5:1 }}>✕ Porte</button>
          <button
            onClick={runWizard}
            title={pendingCount > 0
              ? `${pendingCount} servizi rilevati pronti per essere installati. Clicca per aprire il wizard.`
              : "Wizard rilevamento servizi"}
            disabled={batchBusy}
            style={{
              background: pendingCount > 0 ? "#f59e0b" : tc.accent,
              border: "none",
              borderRadius: 3,
              color: "#fff",
              cursor: batchBusy ? "wait" : "pointer",
              padding: "2px 10px",
              fontSize: 10,
              opacity: batchBusy ? 0.6 : 1,
              display: "flex",
              alignItems: "center",
              gap: 4,
            }}
          >
            <span>+ Configura</span>
            {pendingCount > 0 && (
              <span style={{
                background: "rgba(255,255,255,0.25)",
                borderRadius: 8,
                padding: "0 5px",
                fontSize: 9,
                fontWeight: 700,
                minWidth: 14,
                textAlign: "center",
              }}>{pendingCount}</span>
            )}
          </button>
        </div>
      </div>

      {/* Banner: file modificati dall'ultimo riavvio */}
      {changedFiles.length > 0 && (
        <div style={{
          padding:"6px 12px", borderBottom:`1px solid ${tc.border}`,
          background:"rgba(245, 158, 11, 0.10)",
          display:"flex", alignItems:"center", gap:8,
        }}>
          <span style={{ fontSize:14 }}>⚡</span>
          <div style={{ flex:1, minWidth:0, fontSize:11, color:tc.text }}>
            <strong>{changedFiles.length}</strong> file modificat{changedFiles.length===1?"o":"i"} dall'ultimo riavvio.{" "}
            <span style={{ color:tc.textMuted }}>
              {changedFiles.slice(0,3).map(f=>f.path).join(", ")}
              {changedFiles.length > 3 ? `, +${changedFiles.length-3} altri` : ""}
            </span>
          </div>
          <button
            onClick={handleRestartAll}
            disabled={batchBusy}
            title={services.length===0
              ? "Nessun servizio systemd installato. Clicca per istruzioni su come configurarli."
              : "Riavvia tutti i servizi per recepire le modifiche"}
            style={{
              background:"#f59e0b", color:"#fff", border:"none", borderRadius:3,
              padding:"3px 10px", fontSize:11, cursor:batchBusy?"wait":"pointer",
              flexShrink:0, fontWeight:600,
              opacity:batchBusy?0.5:1,
            }}
          >
            ↻ Riavvia tutti
          </button>
          <button
            onClick={bumpLastRestart}
            title="Ignora queste modifiche (azzera contatore)"
            style={{
              background:"transparent", color:tc.textMuted, border:`1px solid ${tc.border}`,
              borderRadius:3, padding:"3px 8px", fontSize:11, cursor:"pointer", flexShrink:0,
            }}
          >
            ✕
          </button>
        </div>
      )}

      <div style={{ padding:"8px 12px", borderBottom:`1px solid ${tc.border}` }}>
        {services.length === 0 ? (
          managerUnavailable ? (
            <div style={{
              color: "#f59e0b",
              fontSize: 12,
              display: "flex",
              flexDirection: "column",
              gap: 4,
              padding: "4px 0",
            }}>
              <div style={{ display:"flex", alignItems:"center", gap:6, fontWeight:600 }}>
                <span>⚠️</span>
                <span>Manager systemd utente non attivo</span>
              </div>
              <div style={{ color: tc.textSecondary, lineHeight: 1.5 }}>
                {(managerHint ?? "Impossibile elencare i servizi: il bus systemd utente non e' raggiungibile. Avvia il manager o riavvia WSL.")
                  // Rende leggibili i comandi shell wrappati tra backtick letterali (es. `sudo systemctl start user@$(id -u)`).
                  .split("`")
                  .map((seg, i) => (i % 2 === 1
                    ? <code key={i} style={{ fontFamily:'var(--font-mono)', fontSize:11 }}>{seg}</code>
                    : <span key={i}>{seg}</span>))}
              </div>
            </div>
          ) : (
            <div style={{ color:tc.textMuted, fontSize:12 }}>
              {slug
                ? <>Nessun servizio trovato con prefisso <code>{slug}-</code>. Usa <strong>+ Configura</strong> per crearne uno.</>
                : "Caricamento…"}
            </div>
          )
        ) : (
          <>
            {managerUnavailable && (
              <div style={{ fontSize:11, padding:"2px 0 8px", display:"flex", alignItems:"center", gap:6 }}>
                <span style={{ color:"#f59e0b", fontWeight:700 }}>•</span>
                <span style={{ color:tc.textSecondary, lineHeight:1.4 }}>
                  {managerHint ?? "Gestiti in modalita' detached (systemd utente non attivo): avvio, arresto e stato funzionano comunque, senza systemd."}
                </span>
              </div>
            )}
            {services.map(svc => {
          const hasDiag = !!svc.last_error;
          const col = hasDiag ? "#ef4444" : stateColor(svc.state);
          // Match porta↔servizio: prima per campo `service` esatto (popolato dal backend),
          // poi fallback per match testuale su label, infine cache della URL già nota
          // (sopravvive ai cicli di polling in cui il backend non riesce a popolare `service`).
          const svcPort =
            ports.find(p => p.service === svc.short)
            ?? ports.find(p =>
              p.label?.toLowerCase().includes(svc.short.toLowerCase()) ||
              p.label?.toLowerCase().includes(svc.unit.replace(".service","").toLowerCase())
            );
          const cachedUrl = serviceUrlCache[svc.short];
          const effectiveUrl = svcPort?.url ?? cachedUrl;
          const stateText = svc.crash_loop
            ? "crash-loop (si riavvia continuamente)"
            : stateLabel(svc.state, svc.sub);
          const showQuickChat = !!onSendToChat && (svc.state === "failed" || svc.crash_loop) && !svc.last_error;
          return (
            <div key={svc.unit} style={{ marginBottom:6 }}>
              <div style={{ display:"flex",alignItems:"center",gap:8 }}>
                <span style={{ color:col,fontSize:13,flexShrink:0 }}>●</span>
                <span title={svc.unit} style={{ flex:1,minWidth:0,fontSize:12,color:tc.text,fontFamily:'var(--font-mono)',overflow:"hidden",textOverflow:"ellipsis",whiteSpace:"nowrap" }}>
                  {svc.short}
                </span>
                <span style={{ flexShrink:0,fontSize:11,color:col,fontFamily:'var(--font-mono)' }}>
                  {stateText}
                </span>
                {showQuickChat && (
                  <button
                    type="button"
                    onClick={() => {
                      const msg = [
                        `Il servizio "${svc.short}" (unit: ${svc.unit}) è in stato ${svc.state}${svc.crash_loop ? " (crash-loop)" : ""}.`,
                        "",
                        "Richiesta:",
                        `- Esegui/usa: journalctl --user -u ${svc.unit} -n 80 --no-pager`,
                        "- Identifica la causa root e proponi una fix concreta (file/righe).",
                        "- Dammi un test plan minimo per verificare il fix.",
                      ].join("\n");
                      onSendToChat?.(msg);
                    }}
                    title="Invia diagnosi rapida alla chat di Nexus"
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
                      flexShrink: 0,
                    }}
                  >
                    ↗ chat
                  </button>
                )}
                <div style={{ display:"flex",gap:3,flexShrink:0 }}>
                  {(() => {
                    // Mostra solo le azioni sensate per lo stato corrente
                    const isRunning = svc.state === "active" && (svc.sub === "running" || svc.sub === "start");
                    const isExited  = svc.state === "active" && svc.sub === "exited";
                    const isDead    = svc.state === "inactive" || svc.state === "deactivating";
                    const isFailed  = svc.state === "failed";
                    const allowed: ServiceAction[] = isRunning
                      ? ["stop", "restart"]
                      : isFailed
                        ? ["start", "restart"]
                        : (isDead || isExited)
                          ? ["start"]
                          : ["start", "stop", "restart"];
                    return allowed.map(act => {
                      const busy = !!svcBusy[`${svc.unit}-${act}`];
                      const c = act==="start"?"#22c55e":act==="stop"?"#ef4444":"#f59e0b";
                      return <button key={act} disabled={busy} onClick={()=>handleSvcAction(svc,act)} style={actBtnStyle(tc,c,busy)}>{busy?"…":act}</button>;
                    });
                  })()}
                  <button
                    disabled={!!svcBusy[`${svc.unit}-uninstall`]}
                    onClick={()=>handleUninstall(svc)}
                    title={`Disinstalla il servizio (rimuove ${svc.unit} da systemd --user)`}
                    style={{
                      background:"transparent", border:`1px solid ${tc.border}`,
                      color:tc.textMuted, borderRadius:3, padding:"1px 6px", fontSize:10,
                      cursor: svcBusy[`${svc.unit}-uninstall`] ? "wait" : "pointer",
                      fontFamily:'var(--font-mono)',
                      opacity: svcBusy[`${svc.unit}-uninstall`] ? 0.5 : 1,
                    }}
                  >
                    {svcBusy[`${svc.unit}-uninstall`] ? "…" : "🗑"}
                  </button>
                </div>
              </div>
              {effectiveUrl && (
                <div style={{ paddingLeft:21, marginTop:1 }}>
                  <a
                    href={effectiveUrl}
                    target="_blank"
                    rel="noreferrer"
                    style={{ fontSize:10,color:tc.accent,textDecoration:"none",fontFamily:'var(--font-mono)' }}
                    onMouseEnter={e=>(e.currentTarget.style.textDecoration="underline")}
                    onMouseLeave={e=>(e.currentTarget.style.textDecoration="none")}
                  >
                    {effectiveUrl}
                  </a>
                </div>
              )}
              {/* Diagnostica crash-loop: errore, suggerimento e azione AI */}
              {svc.last_error && (() => {
                const isSentForThis = diagSentFor === svc.unit || diagSentFor === svc.short;
                const isPending = isSentForThis && diagResult === "pending";
                const isFailed = isSentForThis && diagResult === "failed";
                return (
                <div style={{
                  paddingLeft:21, marginTop:4, padding:"6px 8px 6px 21px",
                  background:"rgba(239, 68, 68, 0.06)",
                  borderRadius:4,
                  borderLeft:"3px solid #ef4444",
                }}>
                  <div style={{ fontSize:11, color:"#ef4444", fontWeight:600, marginBottom:2 }}>
                    {svc.last_error}
                  </div>
                  {svc.suggestion && (
                    <div style={{ fontSize:11, color:tc.textSecondary, lineHeight:"1.4" }}>
                      {svc.suggestion}
                    </div>
                  )}
                  {isFailed && (
                    <div style={{ fontSize:11, color:"#f59e0b", fontWeight:600, marginTop:4, lineHeight:"1.4" }}>
                      Il problema persiste dopo l'intervento dell'agente. Controlla la risposta nella chat e segui le istruzioni suggerite, oppure riprova.
                    </div>
                  )}
                  {onSendToChat && (
                    <button
                      type="button"
                      disabled={isPending}
                      onClick={() => {
                        const prompt = buildDiagnosticPrompt(svc);
                        setDiagSentFor(svc.unit);
                        setDiagResult("pending");
                        onSendToChat(prompt);
                      }}
                      style={{
                        marginTop:6,
                        background: isPending ? "#6b7280" : tc.accent,
                        color:"#fff", border:"none",
                        borderRadius:4, padding:"4px 12px", fontSize:11, fontWeight:600,
                        cursor: isPending ? "wait" : "pointer",
                        fontFamily:"inherit",
                        display:"inline-flex", alignItems:"center", gap:5,
                        opacity: isPending ? 0.7 : 1,
                      }}
                    >
                      {isPending ? "Agente in esecuzione..." : isFailed ? "Riprova con Nexus" : "Risolvi con Nexus"}
                    </button>
                  )}
                </div>
                );
              })()}
              {/* Feedback: servizio risolto dopo intervento agente */}
              {!svc.last_error && (diagSentFor === svc.unit || diagSentFor === svc.short) && diagResult === "resolved" && (
                <div style={{
                  paddingLeft:21, marginTop:4, padding:"6px 8px 6px 21px",
                  background:"rgba(34, 197, 94, 0.08)",
                  borderRadius:4,
                  borderLeft:"3px solid #22c55e",
                }}>
                  <div style={{ fontSize:11, color:"#22c55e", fontWeight:600 }}>
                    Problema risolto — il servizio e' ora attivo.
                  </div>
                  <button
                    type="button"
                    onClick={() => { setDiagSentFor(null); setDiagResult(null); }}
                    style={{
                      marginTop:4, background:"none", border:"none",
                      color:tc.textMuted, cursor:"pointer", fontSize:10,
                      fontFamily:"inherit", padding:0, textDecoration:"underline",
                    }}
                  >
                    Chiudi
                  </button>
                </div>
              )}
            </div>
          );
        })}
          </>
        )}
        {svcMsg && <div style={{ fontSize:11,color:(svcMsg.toLowerCase().includes("errore")||svcMsg.toLowerCase().includes("error"))?"#ef4444":"#22c55e",marginTop:4 }}>{svcMsg}</div>}
      </div>
    </>
  );
}
