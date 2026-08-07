"use client";

import type { useThemeColors } from "../../../lib/theme";
import type { NexusServiceInfo } from "../../../lib/api-client";
import { stateColor, stateLabel, actBtnStyle, hdrStyle } from "./shared";
import { useI18n } from "../../../lib/i18n";

interface NexusServicesSectionProps {
  tc: ReturnType<typeof useThemeColors>;
  nexusSvcs: NexusServiceInfo[];
  nexusBusy: Record<string, boolean>;
  nexusMsg: string;
  nexusCollapsed: boolean;
  setNexusCollapsed: (fn: (c: boolean) => boolean) => void;
  fetchNexusServices: () => void;
  handleNexusAction: (svc: NexusServiceInfo, action: "start" | "stop" | "restart") => void;
}

export function NexusServicesSection({
  tc,
  nexusSvcs,
  nexusBusy,
  nexusMsg,
  nexusCollapsed,
  setNexusCollapsed,
  fetchNexusServices,
  handleNexusAction,
}: NexusServicesSectionProps) {
  const { t } = useI18n();
  return (
    <>
      {/* ════════════════════════════════ 0: SERVIZI NEXUS ═════════════════ */}
      <div
        style={{ ...hdrStyle(tc), cursor:"pointer", userSelect:"none" }}
        onClick={() => setNexusCollapsed(c => !c)}
      >
        <span style={{ display:"flex", alignItems:"center", gap:5, flex:1, minWidth:0 }}>
          <span style={{ fontSize:9, color:tc.textMuted, transition:"transform 0.15s", display:"inline-block", transform: nexusCollapsed ? "rotate(-90deg)" : "rotate(0deg)" }}>▼</span>
          <span>{t("panels.serviziNexus")}</span>
          {/* Indicatore di stato — visibile anche con sezione compressa */}
          {nexusSvcs.length > 0 && (() => {
            const attivi = nexusSvcs.filter(s => s.state === "active" || s.port_alive).length;
            const totale = nexusSvcs.length;
            const tuttiOk = attivi >= totale;
            return (
              <span style={{
                fontSize:9, fontWeight:600, marginLeft:4,
                color: tuttiOk ? "#22c55e" : "#f59e0b",
                display:"flex", alignItems:"center", gap:3,
              }}>
                <span style={{ width:5, height:5, borderRadius:"50%", background: tuttiOk ? "#22c55e" : "#f59e0b", display:"inline-block", flexShrink:0 }} />
                {tuttiOk
                  ? `operativo (${attivi}/${totale})`
                  : `parziale (${attivi}/${totale})`}
              </span>
            );
          })()}
        </span>
        <button
          onClick={e => { e.stopPropagation(); fetchNexusServices(); }}
          title={t("panels.aggiornaStato")}
          style={{ background:"none",border:`1px solid ${tc.border}`,borderRadius:3,color:tc.textMuted,cursor:"pointer",padding:"1px 8px",fontSize:10 }}
        >↺</button>
      </div>

      {!nexusCollapsed && nexusMsg && (
        <div style={{ padding:"4px 12px", fontSize:11, color: nexusMsg.includes("errore") ? "#ef4444" : "#22c55e", background:"rgba(0,0,0,0.15)", borderBottom:`1px solid ${tc.border}` }}>
          {nexusMsg}
        </div>
      )}

      {!nexusCollapsed && <div style={{ display:"flex", flexDirection:"column", gap:0, borderBottom:`1px solid ${tc.border}` }}>
        {nexusSvcs.length === 0 && (
          <div style={{ padding:"8px 12px", fontSize:11, color:tc.textMuted }}>
            {t("panels.caricamentoServizi")}
          </div>
        )}
        {nexusSvcs.map(svc => {
          const isActive   = svc.state === "active";
          const isFailed   = svc.state === "failed";
          // Processo attivo fuori da systemd: porta risponde ma systemd dice inactive/unknown
          const isPortOnly = !isActive && !isFailed && !!svc.port_alive;
          const dotColor   = stateColor(svc.state, svc.port_alive);
          const stateText  = isActive
            ? `attivo (${svc.sub_state ?? "running"})`
            : stateLabel(svc.state, svc.sub_state ?? "", svc.port_alive);
          const startBusy = nexusBusy[`${svc.name}-start`];
          const stopBusy  = nexusBusy[`${svc.name}-stop`];
          const rstBusy   = nexusBusy[`${svc.name}-restart`];
          const anyBusy   = startBusy || stopBusy || rstBusy;

          return (
            <div key={svc.name} style={{
              display:"flex", alignItems:"center", gap:8,
              padding:"5px 12px", borderBottom:`1px solid ${tc.border}`,
              background:tc.bgCard,
            }}>
              <span
                style={{ width:7, height:7, borderRadius:"50%", background:dotColor, flexShrink:0, display:"inline-block" }}
                title={isPortOnly ? `Attivo sulla porta ${svc.port} (avviato direttamente, non tramite systemd). Usa restart per portarlo sotto systemd.` : undefined}
              />
              <div style={{ flex:1, minWidth:0 }}>
                <span style={{ fontSize:12, color:tc.text, fontWeight:600 }}>{svc.label}</span>
                <span style={{ fontSize:10, color:tc.textMuted, marginLeft:6 }}>:{svc.port}</span>
                {svc.led && (
                  <span title={`Controlla il LED "${svc.led}" nella statusbar`} style={{
                    fontSize:9, color:"#60a5fa", background:"rgba(96,165,250,0.12)",
                    border:"1px solid rgba(96,165,250,0.3)", borderRadius:3,
                    padding:"1px 5px", marginLeft:7, fontFamily:'var(--font-mono)',
                  }}>
                    LED: {svc.led}
                  </span>
                )}
                {isPortOnly && (
                  <span title={t("panels.processoAvviatoDirettamenteFuori")} style={{
                    fontSize:9, color:"#94a3b8", background:"rgba(148,163,184,0.1)",
                    border:"1px solid rgba(148,163,184,0.25)", borderRadius:3,
                    padding:"1px 4px", marginLeft:6, fontFamily:'var(--font-mono)',
                  }}>
                    diretto
                  </span>
                )}
                <span style={{ fontSize:10, color:tc.textMuted, marginLeft:6 }}>
                  {stateText}
                </span>
              </div>
              {!svc.readonly && (
                <div style={{ display:"flex", gap:4, flexShrink:0 }}>
                  {/* "avvia" visibile solo se il processo e' veramente spento (porta non risponde) */}
                  {!isActive && !isPortOnly && (
                    <button
                      onClick={() => handleNexusAction(svc, "start")}
                      disabled={anyBusy}
                      title={`Avvia ${svc.label} tramite systemd`}
                      style={actBtnStyle(tc, "#22c55e", !!anyBusy)}
                    >
                      {startBusy ? "…" : "avvia"}
                    </button>
                  )}
                  {isActive && (
                    <button
                      onClick={() => handleNexusAction(svc, "stop")}
                      disabled={anyBusy}
                      title={`Ferma ${svc.label}`}
                      style={actBtnStyle(tc, "#ef4444", !!anyBusy)}
                    >
                      {stopBusy ? "…" : "stop"}
                    </button>
                  )}
                  <button
                    onClick={() => handleNexusAction(svc, "restart")}
                    disabled={anyBusy}
                    title={isPortOnly ? `Riavvia ${svc.label} tramite systemd (il processo attuale e' fuori da systemd)` : `Riavvia ${svc.label}`}
                    style={actBtnStyle(tc, "#f59e0b", !!anyBusy)}
                  >
                    {rstBusy ? "…" : "restart"}
                  </button>
                </div>
              )}
            </div>
          );
        })}
      </div>}
    </>
  );
}
