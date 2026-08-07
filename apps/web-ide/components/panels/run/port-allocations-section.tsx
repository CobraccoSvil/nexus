"use client";

import type { useThemeColors } from "../../../lib/theme";
import type { PortAllocation } from "../../../lib/api-client";
import { createPortAllocation, deletePortAllocation } from "../../../lib/api-client";
import { hdrStyle } from "./shared";
import { useI18n } from "../../../lib/i18n";

interface PortAllocationsSectionProps {
  tc: ReturnType<typeof useThemeColors>;
  projectId: string;
  portAllocations: PortAllocation[];
  setPortAllocations: React.Dispatch<React.SetStateAction<PortAllocation[]>>;
  showAddPort: boolean;
  setShowAddPort: React.Dispatch<React.SetStateAction<boolean>>;
  newPortValue: string;
  setNewPortValue: React.Dispatch<React.SetStateAction<string>>;
  newPortLabel: string;
  setNewPortLabel: React.Dispatch<React.SetStateAction<string>>;
  portAllocMsg: string;
  setPortAllocMsg: React.Dispatch<React.SetStateAction<string>>;
  fetchPortAllocations: () => void;
}

export function PortAllocationsSection({
  tc,
  projectId,
  portAllocations,
  setPortAllocations,
  showAddPort,
  setShowAddPort,
  newPortValue,
  setNewPortValue,
  newPortLabel,
  setNewPortLabel,
  portAllocMsg,
  setPortAllocMsg,
  fetchPortAllocations,
}: PortAllocationsSectionProps) {
  const { t } = useI18n();
  return (
    <>
      {/* B/B2: lista porte rilevate e allocate spostate nel pannello dedicato
          "Porte" (tab in basso). Qui resta solo la sezione "Porte allocate"
          come strumento gestionale (allocazione manuale + rilascio), per
          gli utenti che lavorano dal pannello Run & Debug. */}

      {/* ════════════════════════════════ B2: PORTE ALLOCATE ═════════════ */}
      <div style={hdrStyle(tc)}>
        <span>Porte allocate ({portAllocations.length})</span>
        <button
          onClick={() => setShowAddPort(!showAddPort)}
          style={{ background:"none",border:"none",color:tc.accent,cursor:"pointer",fontSize:12,fontWeight:600 }}
          title={t("panels.aggiungiPortaManuale")}
        >+</button>
      </div>
      <div style={{ padding:"6px 12px", borderBottom:`1px solid ${tc.border}` }}>
        {portAllocations.length === 0 && !showAddPort && (
          <div style={{ fontSize:10,color:tc.textMuted,fontStyle:"italic" }}>{t("panels.nessunaPortaRegistrata")}</div>
        )}
        {portAllocations.map((a) => (
          <div key={a.id} style={{ display:"flex",alignItems:"center",gap:8,marginBottom:3 }}>
            <span style={{ background:a.allocation_mode==="manual"?"#7c3aed":"#0ea5e9",color:"#fff",borderRadius:3,padding:"1px 6px",fontSize:9,fontFamily:'var(--font-mono)',flexShrink:0,minWidth:48,textAlign:"center" }}>
              {a.port}
            </span>
            <span style={{ fontSize:9,color:tc.textMuted,borderRadius:2,padding:"0 4px",background:a.allocation_mode==="manual"?"rgba(124,58,237,0.1)":"rgba(14,165,233,0.1)" }}>
              {a.allocation_mode}
            </span>
            <span style={{ flex:1,minWidth:0,fontSize:11,color:tc.text,overflow:"hidden",textOverflow:"ellipsis",whiteSpace:"nowrap" }} title={a.label}>
              {a.label || "-"}
            </span>
            <button
              onClick={async () => {
                try {
                  await deletePortAllocation(projectId, a.port);
                  setPortAllocations(prev => prev.filter(x => x.id !== a.id));
                  setPortAllocMsg("");
                } catch (e: unknown) {
                  setPortAllocMsg(`Errore rilascio: ${e instanceof Error ? e.message : String(e)}`);
                }
              }}
              style={{ background:"none",border:"none",color:"#ef4444",cursor:"pointer",fontSize:12,padding:"0 2px",lineHeight:1 }}
              title={t("panels.rilasciaPorta")}
            >&times;</button>
          </div>
        ))}
        {showAddPort && (
          <div style={{ display:"flex",gap:4,alignItems:"center",marginTop:4 }}>
            <input
              type="number"
              min={1024}
              max={65535}
              placeholder={t("panels.porta")}
              value={newPortValue}
              onChange={e => setNewPortValue(e.target.value)}
              style={{ width:64,fontSize:11,padding:"2px 4px",border:`1px solid ${tc.border}`,borderRadius:3,background:tc.bgCard,color:tc.text }}
            />
            <input
              type="text"
              placeholder={t("panels.etichetta")}
              value={newPortLabel}
              onChange={e => setNewPortLabel(e.target.value)}
              style={{ flex:1,fontSize:11,padding:"2px 4px",border:`1px solid ${tc.border}`,borderRadius:3,background:tc.bgCard,color:tc.text }}
            />
            <button
              onClick={async () => {
                const p = parseInt(newPortValue, 10);
                if (!p || p < 1024 || p > 65535) { setPortAllocMsg("Porta non valida (1024-65535)"); return; }
                try {
                  await createPortAllocation(projectId, p, newPortLabel, "manual");
                  setNewPortValue(""); setNewPortLabel(""); setShowAddPort(false); setPortAllocMsg("");
                  fetchPortAllocations();
                } catch (e: unknown) {
                  setPortAllocMsg(e instanceof Error ? e.message : String(e));
                }
              }}
              style={{ fontSize:10,padding:"2px 8px",background:tc.accent,color:"#fff",border:"none",borderRadius:3,cursor:"pointer" }}
            >{t("panels.alloca")}</button>
            <button
              onClick={() => { setShowAddPort(false); setNewPortValue(""); setNewPortLabel(""); setPortAllocMsg(""); }}
              style={{ background:"none",border:"none",color:tc.textMuted,cursor:"pointer",fontSize:12 }}
            >&times;</button>
          </div>
        )}
        {portAllocMsg && <div style={{ fontSize:10,color:"#ef4444",marginTop:3 }}>{portAllocMsg}</div>}
      </div>
    </>
  );
}
