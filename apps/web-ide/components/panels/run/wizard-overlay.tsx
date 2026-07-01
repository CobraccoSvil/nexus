"use client";

import type { useThemeColors } from "../../../lib/theme";
import type { ServiceWizardSuggestion } from "../../../lib/api-client";
import { KIND_ICON, detectRunMode } from "./shared";

interface WizardOverlayProps {
  tc: ReturnType<typeof useThemeColors>;
  suggestions: ServiceWizardSuggestion[];
  wizardLoading: boolean;
  wizardMsg: string;
  setWizardOpen: (v: boolean) => void;
  setInstallingUnit: (svc: ServiceWizardSuggestion) => void;
}

export function WizardOverlay({
  tc,
  suggestions,
  wizardLoading,
  wizardMsg,
  setWizardOpen,
  setInstallingUnit,
}: WizardOverlayProps) {
  return (
    /* ════════════════════════════════ C: WIZARD ═══════════════════════ */
    /* Overlay assoluto: copre l'intero pannello run quando aperto.
        Risolve il problema di spazio insufficiente quando le sezioni
        sopra (Nexus, systemd, porte) occupano tutto il pannello.
        Backdrop semi-trasparente + pannello centrale scrollabile. */
    <div style={{
      position: "absolute",
      inset: 0,
      background: "rgba(0,0,0,0.45)",
      zIndex: 50,
      display: "flex",
      flexDirection: "column",
      padding: "8px",
      overflow: "hidden",
    }}
      onClick={(e) => { if (e.target === e.currentTarget) setWizardOpen(false); }}
    >
      <div style={{
        background: tc.bgSidebar,
        border: `2px solid ${tc.accent}`,
        borderRadius: 8,
        flex: 1,
        display: "flex",
        flexDirection: "column",
        minHeight: 0,
        boxShadow: "0 8px 24px rgba(0,0,0,0.3)",
      }}>
      <div style={{ display:"flex",justifyContent:"space-between",alignItems:"center",padding:"10px 14px",borderBottom:`1px solid ${tc.border}`,background:tc.bgHeader,borderRadius:"6px 6px 0 0" }}>
        <div style={{ fontSize:12,fontWeight:600,color:tc.text }}>Wizard — installa servizi systemd</div>
        <button onClick={()=>setWizardOpen(false)} title="Chiudi wizard" style={{ background:"none",border:"none",color:tc.textMuted,cursor:"pointer",fontSize:18,padding:"0 4px",lineHeight:1 }}>✕</button>
      </div>
      <div style={{ flex:1, overflowY:"auto", padding:"8px 12px" }}>
      <div style={{ fontSize:10,color:tc.textMuted,marginBottom:8,padding:"6px 8px",background:tc.bgCard,border:`1px solid ${tc.border}`,borderRadius:4 }}>
        Crea unit systemd in <code>~/.config/systemd/user/</code> per avviare automaticamente i servizi del progetto. Per comandi <em>on-demand</em> (script di build, test, dev manuale) usa invece la sezione <strong>Run Configurations</strong> nella sidebar a sinistra.
      </div>

      {wizardLoading && <div style={{ color:tc.textMuted,fontSize:12 }}>Analisi in corso…</div>}
      {wizardMsg && <div style={{ fontSize:11,color:wizardMsg.startsWith("✓")?"#22c55e":tc.textMuted,marginBottom:8 }}>{wizardMsg}</div>}

      {/* Raggruppa i suggerimenti per "short" (nome del servizio): cosi' le varianti
          nativo/Docker dello stesso servizio sono insieme e l'utente sceglie. */}
      {(() => {
        // Indice: short -> lista varianti
        const grouped = new Map<string, typeof suggestions>();
        for (const s of suggestions) {
          const arr = grouped.get(s.short) ?? [];
          arr.push(s);
          grouped.set(s.short, arr);
        }
        // Ordina i gruppi per metterci prima quelli installati
        const groups = Array.from(grouped.entries()).sort(([, a], [, b]) => {
          const ai = a.some(x => x.existing) ? 0 : 1;
          const bi = b.some(x => x.existing) ? 0 : 1;
          return ai - bi;
        });
        return groups.map(([short, variants]) => {
          const installedVariant = variants.find(v => v.existing);
          const isInstalled = !!installedVariant;
          // Quando un servizio e' gia' installato mostriamo solo la variante installata
          // e nascondiamo le scelte alternative: la modalita' viene "ricordata".
          const visible = isInstalled ? [installedVariant!] : variants;
          const hasMultipleVariants = !isInstalled && variants.length > 1;
          return (
            <div key={short} style={{
              border: `1px solid ${isInstalled ? "#22c55e" : tc.border}`,
              borderRadius: 6, background: tc.bgCard, marginBottom: 6, padding: 0,
              opacity: isInstalled ? 0.85 : 1,
            }}>
              {hasMultipleVariants && (
                <div style={{
                  fontSize: 10, color: tc.textMuted, padding: "4px 10px 0",
                  fontStyle: "italic",
                }}>
                  Scegli come eseguire <strong>{short}</strong>:
                </div>
              )}
              {visible.map(svc => {
                const mode = detectRunMode(svc.kind, svc.command, svc.args);
                const modeColor = mode === "native" ? "#22c55e" : "#60a5fa";
                const modeLabel = mode === "native" ? "nativo" : "Docker";
                return (
                  <div key={svc.unit} style={{
                    display: "flex", alignItems: "flex-start", gap: 8,
                    padding: "7px 10px",
                    borderTop: visible.length > 1 && svc !== visible[0] ? `1px solid ${tc.border}` : "none",
                  }}>
                    <span style={{ fontSize: 16, flexShrink: 0, marginTop: 1 }}>{KIND_ICON[svc.kind] ?? ""}</span>
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <div style={{ display: "flex", alignItems: "center", gap: 6, flexWrap: "wrap" }}>
                        <span style={{ fontSize: 12, color: tc.text, fontWeight: 600 }}>{svc.label}</span>
                        <span style={{
                          fontSize: 9, color: modeColor,
                          background: `${modeColor}1c`,
                          border: `1px solid ${modeColor}55`,
                          borderRadius: 3, padding: "1px 5px",
                          fontFamily: 'var(--font-mono)',
                          whiteSpace: "nowrap", flexShrink: 0,
                        }}>{modeLabel}</span>
                      </div>
                      <div style={{
                        fontSize: 10, color: tc.textMuted,
                        fontFamily: 'var(--font-mono)',
                        wordBreak: "break-all", overflowWrap: "anywhere",
                      }}>
                        {svc.command} {svc.args.join(" ")}
                      </div>
                      <div style={{
                        fontSize: 10, color: tc.textMuted, overflow: "hidden",
                        textOverflow: "ellipsis", whiteSpace: "nowrap",
                      }}>
                        unit: {svc.unit}
                      </div>
                    </div>
                    {svc.existing ? (
                      <span style={{ fontSize: 11, color: "#22c55e", flexShrink: 0, whiteSpace: "nowrap" }}>
                        ✓ Installato
                      </span>
                    ) : (
                      <button
                        onClick={() => setInstallingUnit(svc)}
                        title={mode === "docker"
                          ? "Esegui come container Docker (richiede dipendenze docker)"
                          : "Esegui direttamente sull'host (sviluppo locale piu' rapido)"}
                        style={{
                          background: mode === "native" ? tc.accent : "rgba(96,165,250,0.15)",
                          border: mode === "native" ? "none" : `1px solid ${modeColor}`,
                          borderRadius: 4, color: mode === "native" ? "#fff" : modeColor,
                          cursor: "pointer", padding: "3px 12px", fontSize: 11, flexShrink: 0,
                        }}
                      >
                        Installa {modeLabel}
                      </button>
                    )}
                  </div>
                );
              })}
            </div>
          );
        });
      })()}

      {!wizardLoading && suggestions.length > 0 && (
        <div style={{ fontSize:10,color:tc.textMuted,marginTop:8 }}>
          I servizi vengono installati come unit systemd --user. Puoi modificarli in <code>~/.config/systemd/user/</code>.
        </div>
      )}
      </div>{/* /scroll area */}
      </div>{/* /pannello centrale */}
    </div>
  );
}
