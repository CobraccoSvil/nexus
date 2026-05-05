"use client";

/**
 * RunPanel — pannello Run & Debug (tab inferiore).
 *
 * Sezione A: Servizi systemd attivi del progetto
 *   - Rilevamento dinamico: tutti i {slug}-*.service installati
 *   - Stato live con polling 5s + bottoni start / stop / restart
 *   - URL/porta cliccabile per ogni servizio in esecuzione
 *
 * Sezione B: Wizard "Configura servizi systemd"
 *   - Analizza il progetto (package.json, .csproj, Cargo.toml, docker-compose…)
 *   - Propone i servizi systemd mancanti
 *   - Installa i file .service e li abilita con un click
 *
 * Nota: le Run Configurations (processi on-demand) sono gestite nella sidebar sinistra.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { useGlobalDialog } from "../global-dialog-provider";
import {
  getProjectServicesStatus,
  controlProjectService,
  getProjectPorts,
  getProjectChanges,
  restartAllProjectServices,
  cleanupProjectPorts,
  detectProjectServices,
  installProjectService,
  uninstallProjectService,
  getNexusServicesStatus,
  controlNexusService,
  getPortAllocations,
  createPortAllocation,
  deletePortAllocation,
  type ProjectServiceEntry,
  type PortEntry,
  type PortAllocation,
  type ServiceWizardSuggestion,
  type NexusServiceInfo,
} from "../../lib/api-client";

interface RunPanelProps {
  projectId: string;
  projectName?: string;
  onSendToChat?: (message: string) => void;
  agentRunEndSignal?: number;
}

type ServiceAction = "start" | "stop" | "restart";

// Cache globale dei servizi Nexus: persiste tra i rimount del pannello.
// Inizializzata a [] e aggiornata ad ogni poll riuscito.
let _nexusSvcsCache: NexusServiceInfo[] = [];

const STATE_COLOR: Record<string, string> = {
  active:       "#22c55e",
  activating:   "#f59e0b",
  deactivating: "#f59e0b",
  inactive:     "#6b7280",
  failed:       "#ef4444",
};

function stateColor(s: string, portAlive?: boolean) {
  // Se il servizio risponde sulla porta, e' operativo a tutti gli effetti -> verde
  if ((s === "inactive" || s === "unknown") && portAlive) return "#22c55e";
  return STATE_COLOR[s] ?? "#6b7280";
}
function stateLabel(state: string, sub: string, portAlive?: boolean) {
  if ((state === "inactive" || state === "unknown") && portAlive) {
    // Servizio attivo ma avviato fuori da systemd (es. via deploy script)
    return "attivo";
  }
  const m: Record<string, string> = { active:"attivo", activating:"avvio…", deactivating:"arresto…", inactive:"inattivo", failed:"errore" };
  const base = m[state] ?? state;
  return sub && sub !== state ? `${base} (${sub})` : base;
}

const KIND_ICON: Record<string, string> = {
  npm:"📦", pnpm:"📦", dotnet:"🔷", cargo:"🦀", python:"🐍", shell:"⚙️", docker:"🐳",
};

/** Determina la modalita' di esecuzione da kind/command/args. */
function detectRunMode(kind: string, command: string, args: string[]): "docker" | "native" {
  if (kind === "docker") return "docker";
  const cmd = (command || "").toLowerCase();
  if (cmd === "docker" || cmd.endsWith("/docker") || cmd === "docker-compose" || cmd.endsWith("/docker-compose")) return "docker";
  // shell wrapper "bash -c 'docker start ...'"
  const joined = `${command} ${args.join(" ")}`.toLowerCase();
  if (/\bdocker\s+(start|run|exec|compose|up|stop|restart)\b/.test(joined)) return "docker";
  return "native";
}

// ── Modale wizard install ──────────────────────────────────────────────────
interface WizardInstallModalProps {
  svc: ServiceWizardSuggestion;
  onInstall: (svc: ServiceWizardSuggestion, env: Record<string,string>, description: string) => Promise<void>;
  onCancel: () => void;
  tc: ReturnType<typeof useThemeColors>;
}

function WizardInstallModal({ svc, onInstall, onCancel, tc }: WizardInstallModalProps) {
  const [env, setEnv] = useState("");
  const [desc, setDesc] = useState(svc.label);
  const [saving, setSaving] = useState(false);

  const inp: React.CSSProperties = {
    width:"100%", background:tc.bgCard, border:`1px solid ${tc.border}`,
    borderRadius:4, color:tc.text, padding:"4px 8px", fontSize:12,
    fontFamily:'"JetBrains Mono", monospace', boxSizing:"border-box",
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
        <div style={{ fontSize:11,color:tc.textMuted,fontFamily:'"JetBrains Mono", monospace',background:tc.bgSidebar,padding:"6px 8px",borderRadius:4 }}>
          <div>Unit: <strong>{svc.unit}</strong></div>
          <div>Comando: {svc.command} {svc.args.join(" ")}</div>
          <div>Dir: {svc.cwd}</div>
        </div>
        <label style={{ fontSize:11,color:tc.textMuted }}>Descrizione</label>
        <input style={inp} value={desc} onChange={e=>setDesc(e.target.value)} />
        <label style={{ fontSize:11,color:tc.textMuted }}>Variabili ambiente extra (KEY=VALUE, una per riga)</label>
        <textarea style={{ ...inp,height:64,resize:"vertical" }} value={env} onChange={e=>setEnv(e.target.value)} placeholder="PORT=3001" />
        <div style={{ fontSize:10,color:tc.textMuted }}>
          Verrà creato <code>~/.config/systemd/user/{svc.unit}</code> e abilitato con systemctl --user enable.
        </div>
        <div style={{ display:"flex",gap:8,justifyContent:"flex-end" }}>
          <button onClick={onCancel} style={{ background:"none",border:`1px solid ${tc.border}`,borderRadius:4,color:tc.textMuted,cursor:"pointer",padding:"4px 14px",fontSize:12 }}>Annulla</button>
          <button onClick={handleInstall} disabled={saving} style={{ background:tc.accent,border:"none",borderRadius:4,color:"#fff",cursor:saving?"wait":"pointer",padding:"4px 14px",fontSize:12,opacity:saving?0.6:1 }}>{saving?"…":"Installa"}</button>
        </div>
      </div>
    </div>
  );
}

// ── Genera prompt diagnostico contestualizzato per la chat AI ─────────────
function buildDiagnosticPrompt(svc: ProjectServiceEntry): string {
  const base = `Il servizio "${svc.short}" (unit: ${svc.unit}) e' in crash-loop e non riesce ad avviarsi.`;
  const error = svc.last_error ? `\nErrore rilevato: ${svc.last_error}` : "";

  switch (svc.error_kind) {
    case "missing_script":
      return `${base}${error}\nControlla il package.json del sotto-progetto corrispondente, identifica lo script corretto per avviare il servizio in modalita' dev e aggiorna il file .service systemd di conseguenza.`;
    case "missing_directory":
      return `${base}${error}\nLa directory di lavoro configurata nel file .service non esiste. Verifica la struttura del progetto e correggi il percorso WorkingDirectory nel file systemd.`;
    case "missing_dependencies":
      return `${base}${error}\nLe dipendenze Node.js non sono installate. Esegui l'installazione delle dipendenze (npm install / pnpm install) nella directory corretta del progetto.`;
    case "missing_sdk":
      return `${base}${error}\nVerifica che il SDK necessario sia installato e accessibile nel PATH. Se non e' installato, suggerisci i comandi per installarlo.`;
    case "build_failed":
      return `${base}${error}\nLeggi il journal del servizio con 'journalctl --user -u ${svc.unit} -n 50 --no-pager' per estrarre gli errori di compilazione completi, poi analizzali e proponi le correzioni necessarie al codice sorgente.`;
    case "port_in_use":
      return `${base}${error}\nIdentifica quale processo occupa la porta e suggerisci come liberarla oppure come configurare il servizio su una porta alternativa.`;
    case "permission_denied":
      return `${base}${error}\nVerifica i permessi dei file coinvolti e suggerisci i comandi necessari per correggerli.`;
    default:
      return `${base}${error}\nAnalizza i log del servizio con 'journalctl --user -u ${svc.unit} -n 50 --no-pager' per identificare la causa del crash e proponi una soluzione.`;
  }
}

// ── Pannello principale ────────────────────────────────────────────────────
export function RunPanel({ projectId, onSendToChat, agentRunEndSignal }: RunPanelProps) {
  const tc = useThemeColors();
  const { confirmDialog } = useGlobalDialog();

  // ── Servizi systemd ──
  const [services,  setServices]  = useState<ProjectServiceEntry[]>([]);
  const [slug,      setSlug]      = useState("");
  const [svcBusy,   setSvcBusy]   = useState<Record<string,boolean>>({});
  const [svcMsg,    setSvcMsg]    = useState("");

  // ── Diagnostica: servizio inviato all'agente ──
  // Traccia quale servizio e' stato inviato all'agente per mostrare feedback
  const [diagSentFor, setDiagSentFor] = useState<string | null>(null);
  // Stato feedback: "pending" = agente in esecuzione, "resolved" = servizio tornato attivo, "failed" = ancora in errore
  const [diagResult, setDiagResult] = useState<"pending" | "resolved" | "failed" | null>(null);

  // ── Porte in ascolto ──
  const [ports, setPorts] = useState<PortEntry[]>([]);

  // ── Porte allocate (registro persistente) ──
  const [portAllocations, setPortAllocations] = useState<PortAllocation[]>([]);
  const [showAddPort, setShowAddPort] = useState(false);
  const [newPortValue, setNewPortValue] = useState("");
  const [newPortLabel, setNewPortLabel] = useState("");
  const [portAllocMsg, setPortAllocMsg] = useState("");

  // Cache delle ultime URL note per ogni servizio. Evita lo "sfarfallio" del link
  // quando il polling porte capita in un istante di transizione (MainPID assente,
  // backend non riesce a popolare il campo `service` per quel ciclo).
  // Si svuota la voce quando il servizio diventa inactive/dead.
  const [serviceUrlCache, setServiceUrlCache] = useState<Record<string, string>>({});

  // ── Auto-restart: file modificati dall'ultimo riavvio (persistito in localStorage) ──
  const lastRestartKey = `nexus.lastRestart.${projectId}`;
  const [lastRestart, setLastRestart] = useState<number>(() => {
    if (typeof window === "undefined") return Date.now();
    const v = window.localStorage.getItem(lastRestartKey);
    const parsed = v ? parseInt(v, 10) : NaN;
    if (Number.isFinite(parsed)) return parsed;
    const now = Date.now();
    window.localStorage.setItem(lastRestartKey, String(now));
    return now;
  });
  const [changedFiles, setChangedFiles] = useState<Array<{ path: string; mtime: number }>>([]);

  const bumpLastRestart = useCallback(() => {
    const now = Date.now();
    setLastRestart(now);
    setChangedFiles([]);
    if (typeof window !== "undefined") {
      window.localStorage.setItem(lastRestartKey, String(now));
    }
  }, [lastRestartKey]);

  const fetchServices = useCallback(async () => {
    try {
      const r = await getProjectServicesStatus(projectId);
      setServices(r.services ?? []);
      setSlug(r.slug ?? "");
    } catch { /* ignora */ }
  }, [projectId]);

  const fetchPorts = useCallback(async () => {
    try {
      const r = await getProjectPorts(projectId);
      setPorts(r.ports ?? []);
    } catch { /* ignora */ }
  }, [projectId]);

  const fetchPortAllocations = useCallback(async () => {
    try {
      const r = await getPortAllocations(projectId);
      setPortAllocations(r.allocations ?? []);
    } catch { /* ignora */ }
  }, [projectId]);

  const fetchChanges = useCallback(async () => {
    try {
      const r = await getProjectChanges(projectId, lastRestart);
      setChangedFiles(r.changed ?? []);
    } catch { /* ignora */ }
  }, [projectId, lastRestart]);

  useEffect(() => {
    fetchServices();
    fetchPorts();
    fetchPortAllocations();
    fetchChanges();
    const t1 = setInterval(fetchServices, 5000);
    const t2 = setInterval(fetchPorts, 6000);
    const t3 = setInterval(fetchChanges, 4000);
    const t4 = setInterval(fetchPortAllocations, 30000);
    return () => { clearInterval(t1); clearInterval(t2); clearInterval(t3); clearInterval(t4); };
  }, [fetchServices, fetchPorts, fetchPortAllocations, fetchChanges]);

  // Quando l'agente completa un run, ri-verifica immediatamente i servizi
  // e aggiorna il feedback diagnostico (risolto / persiste)
  useEffect(() => {
    if (!agentRunEndSignal || !diagSentFor) return;
    // Refresh immediato dei servizi per verificare lo stato post-agente
    const check = async () => {
      try {
        const r = await getProjectServicesStatus(projectId);
        const updated = r.services ?? [];
        setServices(updated);
        const target = updated.find((s) => s.unit === diagSentFor || s.short === diagSentFor);
        if (target) {
          const isOk = target.state === "active" && !target.crash_loop && !target.last_error;
          setDiagResult(isOk ? "resolved" : "failed");
        } else {
          setDiagResult("failed");
        }
      } catch {
        setDiagResult("failed");
      }
    };
    // Attendi 2s per dare tempo al servizio di stabilizzarsi
    const timer = setTimeout(check, 2000);
    return () => clearTimeout(timer);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agentRunEndSignal]);

  // Se il servizio torna attivo durante il polling normale, aggiorna il feedback
  useEffect(() => {
    if (!diagSentFor || diagResult === "resolved") return;
    const target = services.find((s) => s.unit === diagSentFor || s.short === diagSentFor);
    if (target && target.state === "active" && !target.crash_loop && !target.last_error) {
      setDiagResult("resolved");
    }
  }, [services, diagSentFor, diagResult]);

  // Aggiorna il cache delle URL: salva i match trovati, azzera quelli per servizi non più running.
  useEffect(() => {
    setServiceUrlCache(prev => {
      const next: Record<string, string> = {};
      for (const svc of services) {
        const isAlive = svc.state === "active" && (svc.sub === "running" || svc.sub === "exited");
        if (!isAlive) continue; // servizio dead/failed → rimuove dalla cache
        // Match attuale (stessa logica del rendering)
        const p =
          ports.find(pp => pp.service === svc.short)
          ?? ports.find(pp =>
            pp.label?.toLowerCase().includes(svc.short.toLowerCase()) ||
            pp.label?.toLowerCase().includes(svc.unit.replace(".service","").toLowerCase())
          );
        const url = p?.url ?? prev[svc.short];
        if (url) next[svc.short] = url;
      }
      return next;
    });
  }, [services, ports]);

  const handleUninstall = async (svc: ProjectServiceEntry) => {
    if (!await confirmDialog(`Disinstallare il servizio "${svc.short}"?\n\nVerranno eseguiti stop + disable + rimosso il file ~/.config/systemd/user/${svc.unit}.\nL'azione e' reversibile reinstallando il servizio dal wizard.`)) return;
    setSvcBusy(p => ({...p,[`${svc.unit}-uninstall`]:true})); setSvcMsg("");
    try {
      const r = await uninstallProjectService(projectId, svc.short);
      setSvcMsg(r.ok ? `${svc.short}: disinstallato${r.removed ? "" : " (file unit non trovato)"}` : `${svc.short}: errore disinstallazione`);
      setTimeout(()=>{ fetchServices(); fetchPorts(); }, 1000);
    } catch { setSvcMsg(`${svc.short}: errore di rete in disinstallazione`); }
    finally { setSvcBusy(p=>({...p,[`${svc.unit}-uninstall`]:false})); setTimeout(()=>setSvcMsg(""),5000); }
  };

  const handleSvcAction = async (svc: ProjectServiceEntry, action: ServiceAction) => {
    const key = `${svc.unit}-${action}`;
    setSvcBusy(p => ({...p,[key]:true})); setSvcMsg("");
    try {
      const r = await controlProjectService(projectId, svc.short, action);
      setSvcMsg(r.ok ? `${svc.short}: ${action} completato` : `${svc.short}: errore`);
      if (r.ok && (action === "restart" || action === "start")) bumpLastRestart();
      setTimeout(fetchServices, 1200);
    } catch { setSvcMsg(`${svc.short}: errore di rete`); }
    finally { setSvcBusy(p=>({...p,[key]:false})); setTimeout(()=>setSvcMsg(""),5000); }
  };

  const [batchBusy, setBatchBusy] = useState(false);

  const handleRestartAll = async () => {
    if (!await confirmDialog("Riavviare tutti i servizi del progetto?")) return;
    setBatchBusy(true); setSvcMsg("Riavvio in corso…");
    try {
      const r = await restartAllProjectServices(projectId);
      const ok = (r.restarted ?? []).filter(x=>x.ok).length;
      const tot = (r.restarted ?? []).length;
      setSvcMsg(`Riavviati ${ok}/${tot} servizi`);
      if (ok > 0) bumpLastRestart();
      setTimeout(()=>{ fetchServices(); fetchPorts(); }, 1500);
    } catch { setSvcMsg("Errore riavvio batch"); }
    finally { setBatchBusy(false); setTimeout(()=>setSvcMsg(""),6000); }
  };

  const handleCleanupPorts = async () => {
    if (!await confirmDialog("Terminare i processi che occupano porte conflittuali (escludendo quelli gestiti dai servizi del progetto)?")) return;
    setBatchBusy(true); setSvcMsg("Pulizia porte in corso…");
    try {
      const r = await cleanupProjectPorts(projectId);
      const k = (r.killed ?? []).length;
      const s = (r.skipped ?? []).length;
      setSvcMsg(`${k} processi terminati, ${s} protetti (servizi del progetto)`);
      setTimeout(()=>{ fetchServices(); fetchPorts(); }, 1500);
    } catch { setSvcMsg("Errore pulizia porte"); }
    finally { setBatchBusy(false); setTimeout(()=>setSvcMsg(""),8000); }
  };

  // ── Wizard ──
  const [wizardOpen,     setWizardOpen]     = useState(false);
  const [suggestions,    setSuggestions]    = useState<ServiceWizardSuggestion[]>([]);
  const [wizardLoading,  setWizardLoading]  = useState(false);
  const [installingUnit, setInstallingUnit] = useState<ServiceWizardSuggestion|null>(null);
  const [wizardMsg,      setWizardMsg]      = useState("");

  const runWizard = async () => {
    setWizardOpen(true); setWizardLoading(true); setSuggestions([]); setWizardMsg("");
    try {
      const r = await detectProjectServices(projectId);
      setSuggestions(r.suggestions ?? []);
      if ((r.suggestions ?? []).length === 0) setWizardMsg("Nessun servizio rilevato automaticamente. Aggiungi una configurazione manualmente.");
    } catch { setWizardMsg("Errore durante il rilevamento. Controlla che il backend sia raggiungibile."); }
    finally { setWizardLoading(false); }
  };

  const handleInstall = async (svc: ServiceWizardSuggestion, env: Record<string,string>, description: string) => {
    setInstallingUnit(null);
    try {
      const r = await installProjectService(projectId, { ...svc, env, description });
      if (r.ok) {
        setSuggestions(prev => prev.map(s => s.unit===svc.unit ? {...s,existing:true} : s));
        setWizardMsg(`✓ ${svc.unit} installato.`);
        fetchServices();
      } else {
        setWizardMsg(`Errore durante l'installazione di ${svc.unit}.`);
      }
    } catch { setWizardMsg("Errore di rete."); }
  };

  // ── Servizi Nexus ──
  // Inizializza dalla cache globale: se il pannello si rimonta, mostra subito
  // i dati dell'ultimo poll invece di "Caricamento servizi…".
  const [nexusSvcs,     setNexusSvcs]     = useState<NexusServiceInfo[]>(_nexusSvcsCache);
  const [nexusBusy,     setNexusBusy]     = useState<Record<string,boolean>>({});
  const [nexusMsg,      setNexusMsg]      = useState("");
  // Sezione Nexus collassata di default: lo stato sintetico (operativo X/Y)
  // resta visibile nell'header, l'utente la apre solo se vuole controlli puntuali
  const [nexusCollapsed, setNexusCollapsed] = useState(true);
  const nexusMounted = useRef(true);
  useEffect(() => { nexusMounted.current = true; return () => { nexusMounted.current = false; }; }, []);

  const fetchNexusServices = useCallback(async () => {
    try {
      const r = await getNexusServicesStatus();
      const svcs = r.services ?? [];
      _nexusSvcsCache = svcs;          // aggiorna cache globale
      if (nexusMounted.current) setNexusSvcs(svcs);
    } catch { /* ignora — il backend potrebbe essere down */ }
  }, []);

  useEffect(() => {
    fetchNexusServices();
    const t = setInterval(fetchNexusServices, 8000);
    return () => clearInterval(t);
  }, [fetchNexusServices]);

  const handleNexusAction = async (svc: NexusServiceInfo, action: "start" | "stop" | "restart") => {
    const key = `${svc.name}-${action}`;
    if (nexusMounted.current) { setNexusBusy(p => ({ ...p, [key]: true })); setNexusMsg(""); }
    try {
      const r = await controlNexusService(svc.name, action);
      if (nexusMounted.current) setNexusMsg(r.ok ? `${svc.label}: ${action} completato` : `${svc.label}: ${r.stderr || "errore"}`);
      setTimeout(fetchNexusServices, 1500);
    } catch { if (nexusMounted.current) setNexusMsg(`${svc.label}: errore di rete`); }
    finally {
      if (nexusMounted.current) setNexusBusy(p => ({ ...p, [key]: false }));
      setTimeout(() => { if (nexusMounted.current) setNexusMsg(""); }, 6000);
    }
  };

  // ── Layout ──
  const hdr = (extra?: React.CSSProperties): React.CSSProperties => ({
    fontSize:10, color:tc.textMuted, textTransform:"uppercase", letterSpacing:"0.08em",
    padding:"8px 12px 6px", borderBottom:`1px solid ${tc.border}`,
    background:tc.bgSidebar, flexShrink:0,
    display:"flex", justifyContent:"space-between", alignItems:"center",
    ...extra,
  });

  const actBtn = (color:string, busy:boolean): React.CSSProperties => ({
    background:"transparent", border:`1px solid ${color}`, color:busy?tc.textMuted:color,
    borderRadius:3, padding:"1px 7px", fontSize:10, cursor:busy?"wait":"pointer",
    fontFamily:'"JetBrains Mono", monospace', opacity:busy?0.5:1, transition:"opacity 0.15s",
  });

  return (
    <div style={{ display:"flex", flexDirection:"column", height:"100%", minHeight:0, overflow:"auto" }}>

      {/* ════════════════════════════════ 0: SERVIZI NEXUS ═════════════════ */}
      <div
        style={{ ...hdr(), cursor:"pointer", userSelect:"none" }}
        onClick={() => setNexusCollapsed(c => !c)}
      >
        <span style={{ display:"flex", alignItems:"center", gap:5, flex:1, minWidth:0 }}>
          <span style={{ fontSize:9, color:tc.textMuted, transition:"transform 0.15s", display:"inline-block", transform: nexusCollapsed ? "rotate(-90deg)" : "rotate(0deg)" }}>▼</span>
          <span>Servizi Nexus</span>
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
          title="Aggiorna stato"
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
            Caricamento servizi…
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
                    padding:"1px 5px", marginLeft:7, fontFamily:'"JetBrains Mono", monospace',
                  }}>
                    LED: {svc.led}
                  </span>
                )}
                {isPortOnly && (
                  <span title="Processo avviato direttamente (fuori da systemd)" style={{
                    fontSize:9, color:"#94a3b8", background:"rgba(148,163,184,0.1)",
                    border:"1px solid rgba(148,163,184,0.25)", borderRadius:3,
                    padding:"1px 4px", marginLeft:6, fontFamily:'"JetBrains Mono", monospace',
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
                      style={actBtn("#22c55e", !!anyBusy)}
                    >
                      {startBusy ? "…" : "avvia"}
                    </button>
                  )}
                  {isActive && (
                    <button
                      onClick={() => handleNexusAction(svc, "stop")}
                      disabled={anyBusy}
                      title={`Ferma ${svc.label}`}
                      style={actBtn("#ef4444", !!anyBusy)}
                    >
                      {stopBusy ? "…" : "stop"}
                    </button>
                  )}
                  <button
                    onClick={() => handleNexusAction(svc, "restart")}
                    disabled={anyBusy}
                    title={isPortOnly ? `Riavvia ${svc.label} tramite systemd (il processo attuale e' fuori da systemd)` : `Riavvia ${svc.label}`}
                    style={actBtn("#f59e0b", !!anyBusy)}
                  >
                    {rstBusy ? "…" : "restart"}
                  </button>
                </div>
              )}
            </div>
          );
        })}
      </div>}

      {/* ════════════════════════════════ A: SERVIZI SYSTEMD ══════════════ */}
      <div style={hdr()} title="Servizi systemd persistenti del progetto: vivono in ~/.config/systemd/user/, partono al boot se enabled, vengono riavviati dal sistema se crashano.">
        <span>Servizi systemd persistenti{slug ? ` — ${slug}` : ""}</span>
        <div style={{ display:"flex", gap:6 }}>
          <button onClick={fetchServices} title="Aggiorna stato" disabled={batchBusy} style={{ background:"none",border:`1px solid ${tc.border}`,borderRadius:3,color:tc.textMuted,cursor:batchBusy?"wait":"pointer",padding:"1px 8px",fontSize:10 }}>↺</button>
          <button onClick={handleRestartAll} title="Riavvia tutti i servizi del progetto" disabled={batchBusy || services.length===0} style={{ background:"transparent",border:`1px solid #f59e0b`,borderRadius:3,color:"#f59e0b",cursor:batchBusy?"wait":"pointer",padding:"1px 8px",fontSize:10,opacity:(batchBusy||services.length===0)?0.5:1 }}>↻ Tutti</button>
          <button onClick={handleCleanupPorts} title="Termina processi su porte conflittuali (esclude i servizi del progetto)" disabled={batchBusy} style={{ background:"transparent",border:`1px solid #ef4444`,borderRadius:3,color:"#ef4444",cursor:batchBusy?"wait":"pointer",padding:"1px 8px",fontSize:10,opacity:batchBusy?0.5:1 }}>✕ Porte</button>
          <button onClick={runWizard} title="Wizard rilevamento servizi" disabled={batchBusy} style={{ background:tc.accent,border:"none",borderRadius:3,color:"#fff",cursor:batchBusy?"wait":"pointer",padding:"2px 10px",fontSize:10,opacity:batchBusy?0.6:1 }}>+ Configura</button>
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
            disabled={batchBusy || services.length===0}
            title="Riavvia tutti i servizi per recepire le modifiche"
            style={{
              background:"#f59e0b", color:"#fff", border:"none", borderRadius:3,
              padding:"3px 10px", fontSize:11, cursor:batchBusy?"wait":"pointer",
              flexShrink:0, fontWeight:600,
              opacity:(batchBusy||services.length===0)?0.5:1,
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
          <div style={{ color:tc.textMuted, fontSize:12 }}>
            {slug
              ? <>Nessun servizio trovato con prefisso <code>{slug}-</code>. Usa <strong>+ Configura</strong> per crearne uno.</>
              : "Caricamento…"}
          </div>
        ) : services.map(svc => {
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
          return (
            <div key={svc.unit} style={{ marginBottom:6 }}>
              <div style={{ display:"flex",alignItems:"center",gap:8 }}>
                <span style={{ color:col,fontSize:13,flexShrink:0 }}>●</span>
                <span title={svc.unit} style={{ flex:1,minWidth:0,fontSize:12,color:tc.text,fontFamily:'"JetBrains Mono", monospace',overflow:"hidden",textOverflow:"ellipsis",whiteSpace:"nowrap" }}>
                  {svc.short}
                </span>
                <span style={{ flexShrink:0,fontSize:11,color:col,fontFamily:'"JetBrains Mono", monospace' }}>
                  {stateText}
                </span>
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
                      return <button key={act} disabled={busy} onClick={()=>handleSvcAction(svc,act)} style={actBtn(c,busy)}>{busy?"…":act}</button>;
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
                      fontFamily:'"JetBrains Mono", monospace',
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
                    style={{ fontSize:10,color:tc.accent,textDecoration:"none",fontFamily:'"JetBrains Mono", monospace' }}
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
        {svcMsg && <div style={{ fontSize:11,color:(svcMsg.toLowerCase().includes("errore")||svcMsg.toLowerCase().includes("error"))?"#ef4444":"#22c55e",marginTop:4 }}>{svcMsg}</div>}
      </div>

      {/* ════════════════════════════════ B: PORTE RILEVATE ═══════════════ */}
      {ports.length > 0 && (
        <>
          <div style={hdr()}>
            <span>Porte rilevate ({ports.length})</span>
          </div>
          <div style={{ padding:"6px 12px", borderBottom:`1px solid ${tc.border}` }}>
            {ports.map((p,i) => (
              <div key={`${p.port}-${i}`} style={{ display:"flex",alignItems:"center",gap:8,marginBottom:3 }}>
                <span style={{ background:tc.accentBg,color:tc.accent,borderRadius:3,padding:"1px 6px",fontSize:10,fontFamily:'"JetBrains Mono", monospace',flexShrink:0,minWidth:48,textAlign:"center" }}>
                  {p.port}
                </span>
                <span style={{ flex:1,minWidth:0,fontSize:11,color:tc.text,overflow:"hidden",textOverflow:"ellipsis",whiteSpace:"nowrap" }} title={p.label}>
                  {p.label || `(processo ${p.port})`}
                </span>
                {p.url && (
                  <a href={p.url} target="_blank" rel="noreferrer"
                    style={{ fontSize:10,color:tc.accent,textDecoration:"none",fontFamily:'"JetBrains Mono", monospace',flexShrink:0 }}
                    onMouseEnter={e=>(e.currentTarget.style.textDecoration="underline")}
                    onMouseLeave={e=>(e.currentTarget.style.textDecoration="none")}>
                    apri ↗
                  </a>
                )}
              </div>
            ))}
          </div>
        </>
      )}

      {/* ════════════════════════════════ B2: PORTE ALLOCATE ═════════════ */}
      <div style={hdr()}>
        <span>Porte allocate ({portAllocations.length})</span>
        <button
          onClick={() => setShowAddPort(!showAddPort)}
          style={{ background:"none",border:"none",color:tc.accent,cursor:"pointer",fontSize:12,fontWeight:600 }}
          title="Aggiungi porta manuale"
        >+</button>
      </div>
      <div style={{ padding:"6px 12px", borderBottom:`1px solid ${tc.border}` }}>
        {portAllocations.length === 0 && !showAddPort && (
          <div style={{ fontSize:10,color:tc.textMuted,fontStyle:"italic" }}>Nessuna porta registrata</div>
        )}
        {portAllocations.map((a) => (
          <div key={a.id} style={{ display:"flex",alignItems:"center",gap:8,marginBottom:3 }}>
            <span style={{ background:a.allocation_mode==="manual"?"#7c3aed":"#0ea5e9",color:"#fff",borderRadius:3,padding:"1px 6px",fontSize:9,fontFamily:'"JetBrains Mono", monospace',flexShrink:0,minWidth:48,textAlign:"center" }}>
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
              title="Rilascia porta"
            >&times;</button>
          </div>
        ))}
        {showAddPort && (
          <div style={{ display:"flex",gap:4,alignItems:"center",marginTop:4 }}>
            <input
              type="number"
              min={1024}
              max={65535}
              placeholder="Porta"
              value={newPortValue}
              onChange={e => setNewPortValue(e.target.value)}
              style={{ width:64,fontSize:11,padding:"2px 4px",border:`1px solid ${tc.border}`,borderRadius:3,background:tc.bgCard,color:tc.text }}
            />
            <input
              type="text"
              placeholder="Etichetta"
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
            >Alloca</button>
            <button
              onClick={() => { setShowAddPort(false); setNewPortValue(""); setNewPortLabel(""); setPortAllocMsg(""); }}
              style={{ background:"none",border:"none",color:tc.textMuted,cursor:"pointer",fontSize:12 }}
            >&times;</button>
          </div>
        )}
        {portAllocMsg && <div style={{ fontSize:10,color:"#ef4444",marginTop:3 }}>{portAllocMsg}</div>}
      </div>

      {/* ════════════════════════════════ C: WIZARD ═══════════════════════ */}
      {wizardOpen && (
        <div style={{ flex:1,overflow:"auto",padding:"8px 12px" }}>
          <div style={{ display:"flex",justifyContent:"space-between",alignItems:"center",marginBottom:8 }}>
            <div style={{ fontSize:11,fontWeight:600,color:tc.text }}>Wizard — installa servizi systemd</div>
            <button onClick={()=>setWizardOpen(false)} style={{ background:"none",border:"none",color:tc.textMuted,cursor:"pointer",fontSize:14 }}>✕</button>
          </div>
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
                              fontFamily: '"JetBrains Mono", monospace',
                              whiteSpace: "nowrap", flexShrink: 0,
                            }}>{modeLabel}</span>
                          </div>
                          <div style={{
                            fontSize: 10, color: tc.textMuted,
                            fontFamily: '"JetBrains Mono", monospace',
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
        </div>
      )}

      {/* Modali */}
      {installingUnit && (
        <WizardInstallModal
          svc={installingUnit}
          onInstall={handleInstall}
          onCancel={()=>setInstallingUnit(null)}
          tc={tc}
        />
      )}
    </div>
  );
}
