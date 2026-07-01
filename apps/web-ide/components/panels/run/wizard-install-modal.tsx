"use client";

import { useEffect, useState } from "react";
import type { useThemeColors } from "../../../lib/theme";
import type { ServiceWizardSuggestion } from "../../../lib/api-client";

// ── Modale wizard install ──────────────────────────────────────────────────
interface WizardInstallModalProps {
  svc: ServiceWizardSuggestion;
  onInstall: (svc: ServiceWizardSuggestion, env: Record<string,string>, description: string) => Promise<void>;
  onCancel: () => void;
  tc: ReturnType<typeof useThemeColors>;
  /** Feedback dall'ultimo tentativo: stringa libera (es. "✓ taskboard-frontend.service installato" o
   *  "Errore: ..."). Quando inizia con "✓" o "OK" la modale si auto-chiude dopo 1.2s. */
  feedback?: string;
}

export function WizardInstallModal({ svc, onInstall, onCancel, tc, feedback }: WizardInstallModalProps) {
  const [env, setEnv] = useState(() => {
    const obj = (svc.env ?? {}) as Record<string, string>;
    const lines = Object.entries(obj)
      .filter(([k, v]) => k && v != null && String(v).length > 0)
      .map(([k, v]) => `${k}=${v}`);
    return lines.join("\n");
  });
  const [desc, setDesc] = useState(svc.label);
  const [saving, setSaving] = useState(false);
  // Auto-close su successo
  useEffect(() => {
    if (!feedback) return;
    const isOk = feedback.startsWith("✓") || feedback.toUpperCase().startsWith("OK");
    if (isOk) {
      const t = setTimeout(() => onCancel(), 1200);
      return () => clearTimeout(t);
    }
  }, [feedback, onCancel]);

  const inp: React.CSSProperties = {
    width:"100%", background:tc.bgCard, border:`1px solid ${tc.border}`,
    borderRadius:4, color:tc.text, padding:"4px 8px", fontSize:12,
    fontFamily:'var(--font-mono)', boxSizing:"border-box",
  };

  const handleInstall = async () => {
    setSaving(true);
    const envObj: Record<string,string> = {};
    env.split("\n").forEach(l => { const i=l.indexOf("="); if(i>0) envObj[l.slice(0,i).trim()]=l.slice(i+1).trim(); });
    await onInstall(svc, envObj, desc);
    setSaving(false);
  };

  return (
    <div style={{ position:"fixed",inset:0,zIndex:9999,background:"rgba(0,0,0,0.55)",display:"flex",alignItems:"center",justifyContent:"center" }}>
      <div style={{ background:tc.bgCard,border:`1px solid ${tc.border}`,borderRadius:8,padding:20,width:460,maxWidth:"90vw",display:"flex",flexDirection:"column",gap:10 }}>
        <div style={{ fontWeight:700,fontSize:13,color:tc.text }}>Installa servizio — {svc.short}</div>
        <div style={{ fontSize:11,color:tc.textMuted,fontFamily:'var(--font-mono)',background:tc.bgSidebar,padding:"6px 8px",borderRadius:4 }}>
          <div>Unit: <strong>{svc.unit}</strong></div>
          <div>Comando: {svc.command} {svc.args.join(" ")}</div>
          <div>Dir: {svc.cwd}</div>
        </div>
        <label style={{ fontSize:11,color:tc.textMuted }}>Descrizione</label>
        <input style={inp} value={desc} onChange={e=>setDesc(e.target.value)} />
        <label style={{ fontSize:11,color:tc.textMuted }}>Variabili ambiente (KEY=VALUE, una per riga)</label>
        <textarea style={{ ...inp,height:64,resize:"vertical" }} value={env} onChange={e=>setEnv(e.target.value)} placeholder="PORT=20000" />
        <div style={{ fontSize:10,color:tc.textMuted }}>
          Verrà creato <code>~/.config/systemd/user/{svc.unit}</code> e abilitato con systemctl --user enable.
        </div>
        {/* Banner feedback: mostrato dopo il tentativo di install.
            - "✓"/"OK" → verde, la modale si chiude da sola dopo 1.2s.
            - altro → rosso, la modale resta aperta cosi' l'utente puo' correggere. */}
        {feedback && (
          <div
            style={{
              fontSize: 11,
              padding: "6px 8px",
              borderRadius: 4,
              background: feedback.startsWith("✓") ? "#22c55e20" : `${tc.error}20`,
              border: `1px solid ${feedback.startsWith("✓") ? "#22c55e60" : `${tc.error}60`}`,
              color: feedback.startsWith("✓") ? "#22c55e" : tc.error,
              whiteSpace: "pre-wrap",
              wordBreak: "break-word",
            }}
          >
            {feedback}
          </div>
        )}
        <div style={{ display:"flex",gap:8,justifyContent:"flex-end" }}>
          <button onClick={onCancel} style={{ background:"none",border:`1px solid ${tc.border}`,borderRadius:4,color:tc.textMuted,cursor:"pointer",padding:"4px 14px",fontSize:12 }}>{feedback && !feedback.startsWith("✓") ? "Chiudi" : "Annulla"}</button>
          <button onClick={handleInstall} disabled={saving} style={{ background:tc.accent,border:"none",borderRadius:4,color:"#fff",cursor:saving?"wait":"pointer",padding:"4px 14px",fontSize:12,opacity:saving?0.6:1 }}>{saving?"installazione…":(feedback && !feedback.startsWith("✓") ? "Riprova" : "Installa")}</button>
        </div>
      </div>
    </div>
  );
}
