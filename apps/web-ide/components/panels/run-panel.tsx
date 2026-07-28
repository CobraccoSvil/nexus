"use client";

/**
 * RunPanel — pannello Run & Debug (tab inferiore).
 *
 * Sezione A: Servizi del progetto attivi
 *   - Rilevamento dinamico dei servizi gestiti del progetto
 *   - Stato live via SSE dispatcher (ServiceStatusChanged + operationalRefresh)
 *   - URL/porta cliccabile per ogni servizio in esecuzione
 *
 * Sezione B: Wizard "Configura servizi del progetto"
 *   - Analizza il progetto (package.json, .csproj, Cargo.toml, docker-compose…)
 *   - Propone i servizi del progetto mancanti
 *   - Registra i servizi e li avvia con un click
 *
 * Nota: le Run Configurations (processi on-demand) sono gestite nella sidebar sinistra.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { useThemeColors } from "../../lib/theme";
import { useGlobalDialog } from "../global-dialog-provider";
import {
  useProjectStore,
  selectOperationalRefreshAt,
} from "../../lib/project-dispatcher/store";
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
  type ProjectServiceEntry,
  type PortEntry,
  type PortAllocation,
  type ServiceWizardSuggestion,
  type NexusServiceInfo,
} from "../../lib/api-client";
import { type ServiceAction } from "./run/shared";
import { WizardInstallModal } from "./run/wizard-install-modal";
import { NexusServicesSection } from "./run/nexus-services-section";
import { ProjectServicesSection } from "./run/project-services-section";
import { StaticSiteSection } from "./run/static-site-section";
import { PortAllocationsSection } from "./run/port-allocations-section";
import { WizardOverlay } from "./run/wizard-overlay";

/**
 * Conteggio dei servizi rilevati dal wizard ancora DA configurare, per il badge
 * "+ Configura". Esclude:
 *  1. i candidati gia' marcati `existing` dal backend;
 *  2. quelli GIA' gestiti dal progetto (presenti tra `managed` per unit o short),
 *     anche quando il gestore non e' interrogabile: in quel caso il backend
 *     `mark_existing_services` e' cieco e li conterebbe a torto;
 *  3. i duplicati (stesso unit/short, es. lo script alias `dev` -> `dev:frontend`).
 * Cosi' il badge non dice "3 da configurare" quando 2 sono gia' in lista e 1 e' un doppione.
 */
function pendingServicesCount(
  suggestions: ServiceWizardSuggestion[],
  managed: ProjectServiceEntry[],
): number {
  const managedUnits = new Set(managed.map((s) => s.unit).filter(Boolean));
  const managedShorts = new Set(managed.map((s) => s.short).filter(Boolean));
  const seen = new Set<string>();
  let n = 0;
  for (const s of suggestions) {
    if (s.existing) continue;
    if ((s.unit && managedUnits.has(s.unit)) || (s.short && managedShorts.has(s.short))) continue;
    const key = s.unit || s.short || "";
    if (key && seen.has(key)) continue;
    if (key) seen.add(key);
    n += 1;
  }
  return n;
}

interface RunPanelProps {
  projectId: string;
  projectName?: string;
  onSendToChat?: (message: string) => void;
  agentRunEndSignal?: number;
}

// Cache globale dei servizi Nexus: persiste tra i rimount del pannello.
// Inizializzata a [] e aggiornata ad ogni poll riuscito.
let _nexusSvcsCache: NexusServiceInfo[] = [];

// ── Pannello principale ────────────────────────────────────────────────────
export function RunPanel({ projectId, onSendToChat, agentRunEndSignal }: RunPanelProps) {
  const tc = useThemeColors();
  const { confirmDialog } = useGlobalDialog();

  // ── Servizi del progetto ──
  const [services,  setServices]  = useState<ProjectServiceEntry[]>([]);
  const [slug,      setSlug]      = useState("");
  // ADR 0022: gestore dei servizi irraggiungibile (servizi presenti ma non elencabili).
  const [managerUnavailable, setManagerUnavailable] = useState(false);
  const [managerHint, setManagerHint] = useState<string | undefined>(undefined);
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

  // Qui viveva una cache delle ultime URL note per servizio, nata per evitare lo
  // "sfarfallio" del link nei cicli di polling in cui il backend non popolava
  // `service`. Rimossa: curava un tremolio e in cambio faceva sopravvivere un
  // indirizzo alla propria allocazione, cioe' mostrava una porta che il servizio
  // non aveva (piu'). Un link che appare e sparisce e' fastidioso; un link che
  // resta e mente manda a debuggare la porta sbagliata. Ora l'URL lo mostra solo
  // chi ha un'allocazione legata alla propria identita' (project-services-section).

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
      setManagerUnavailable(r.manager_unavailable ?? false);
      setManagerHint(r.manager_hint);
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
    void fetchServices();
    void fetchPorts();
    void fetchPortAllocations();
    void fetchChanges();
  }, [fetchServices, fetchPorts, fetchPortAllocations, fetchChanges]);

  const operationalRefreshAt = useProjectStore(selectOperationalRefreshAt);
  useEffect(() => {
    if (operationalRefreshAt === 0) return;
    void fetchServices();
    void fetchPorts();
    void fetchPortAllocations();
    void fetchChanges();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [operationalRefreshAt]);

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

  const handleUninstall = async (svc: ProjectServiceEntry) => {
    if (!await confirmDialog(`Disinstallare il servizio "${svc.short}"?\n\nIl servizio verra' fermato e rimosso dal progetto.\nL'azione e' reversibile reinstallando il servizio dal wizard.`)) return;
    setSvcBusy(p => ({...p,[`${svc.unit}-uninstall`]:true})); setSvcMsg("");
    try {
      const r = await uninstallProjectService(projectId, svc.short);
      setSvcMsg(r.ok ? `${svc.short}: disinstallato${r.removed ? "" : " (nessun elemento da rimuovere)"}` : `${svc.short}: errore disinstallazione`);
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
    // Caso 1: nessun servizio del progetto configurato (M47).
    if (services.length === 0) {
      await confirmDialog(
        "Nessun servizio del progetto configurato. " +
        "Usa il pulsante '+ Configura' qui sopra per lanciare il wizard di rilevamento, " +
        "oppure avvia i Run Configurations dal pannello Run & Debug (npm dev / npm start).",
      );
      bumpLastRestart(); // Azzera comunque il contatore "file modificati" se l'utente conferma
      return;
    }
    // Caso 2: gestore dei servizi non interrogabile in batch. Il restart batch
    // via endpoint restart-all ritornerebbe 0 servizi ("non succede nulla" lato
    // utente, come segnalato). Ma i servizi SONO riavviabili individualmente:
    // controlProjectService li gestisce, come i pulsanti "restart" singoli. Li
    // riavviamo quindi in batch DALLA UI, senza rimandare l'utente al terminale
    // (regola: tutto gestibile da Nexus).
    if (managerUnavailable) {
      if (!await confirmDialog("Riavviare tutti i servizi del progetto?")) return;
      setBatchBusy(true); setSvcMsg("Riavvio in corso…");
      let ok = 0;
      for (const svc of services) {
        try {
          const r = await controlProjectService(projectId, svc.short, "restart");
          if (r.ok) ok += 1;
        } catch { /* continua col prossimo servizio */ }
      }
      setSvcMsg(`Riavviati ${ok}/${services.length} servizi`);
      if (ok > 0) bumpLastRestart();
      setTimeout(() => { fetchServices(); fetchPorts(); }, 1500);
      setBatchBusy(false); setTimeout(() => setSvcMsg(""), 10000);
      return;
    }
    // Caso 3: gestore attivo -> restart batch via endpoint dedicato.
    if (!await confirmDialog("Riavviare tutti i servizi del progetto?")) return;
    setBatchBusy(true); setSvcMsg("Riavvio in corso…");
    try {
      const r = await restartAllProjectServices(projectId);
      const ok = (r.restarted ?? []).filter(x=>x.ok).length;
      const tot = (r.restarted ?? []).length;
      // Edge case: l'endpoint ritorna 0 servizi (slug non matcha, gestore appena
      // morto) — segnalare invece di lasciare "Riavviati 0/0 servizi" criptico.
      if (tot === 0) {
        setSvcMsg("Nessun servizio del progetto trovato (lo slug non corrisponde ai servizi registrati).");
      } else {
        setSvcMsg(`Riavviati ${ok}/${tot} servizi`);
        if (ok > 0) bumpLastRestart();
      }
      setTimeout(()=>{ fetchServices(); fetchPorts(); }, 1500);
    } catch (e) {
      // Mostra l'errore reale invece del generico "Errore riavvio batch".
      const msg = e instanceof Error ? e.message : String(e);
      setSvcMsg(`Errore riavvio batch: ${msg.slice(0, 120)}`);
    }
    finally { setBatchBusy(false); setTimeout(()=>setSvcMsg(""),10000); }
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
  // Conteggio servizi rilevati ma non installati. Calcolato dal wizard detect
  // al mount + ogni 60s. Mostrato come badge accanto al pulsante "+ Configura"
  // per segnalare all'utente che ci sono servizi da configurare.
  const [pendingCount,   setPendingCount]   = useState(0);

  const runWizard = async () => {
    setWizardOpen(true); setWizardLoading(true); setSuggestions([]); setWizardMsg("");
    try {
      const r = await detectProjectServices(projectId);
      const items = r.suggestions ?? [];
      setSuggestions(items);
      setPendingCount(pendingServicesCount(items, services));
      if (items.length === 0) setWizardMsg("Nessun servizio rilevato automaticamente. Aggiungi una configurazione manualmente.");
    } catch { setWizardMsg("Errore durante il rilevamento. Controlla che il backend sia raggiungibile."); }
    finally { setWizardLoading(false); }
  };

  // Ref a `services` per leggerlo dentro l'interval senza ricrearlo a ogni
  // aggiornamento (il polling rinfresca `services` di continuo).
  const servicesRef = useRef(services);
  useEffect(() => { servicesRef.current = services; }, [services]);

  // Auto-fetch al mount + ogni 60s per popolare il badge "pending".
  // Niente UI: solo conteggio. L'utente apre il wizard manualmente quando vede il badge.
  useEffect(() => {
    let cancelled = false;
    const tick = async () => {
      try {
        const r = await detectProjectServices(projectId);
        if (!cancelled) {
          setPendingCount(pendingServicesCount(r.suggestions ?? [], servicesRef.current));
        }
      } catch { /* ignora errori in background */ }
    };
    void tick();
    return () => { cancelled = true; };
  }, [projectId, operationalRefreshAt]);

  const handleInstall = async (svc: ServiceWizardSuggestion, env: Record<string,string>, description: string) => {
    // Non chiudere la modale prima di sapere l'esito: l'utente deve vedere
    // se l'installazione e' andata a buon fine o no. Inoltre, anche quando
    // il backend risponde ok=false, ri-eseguiamo il detect cosi' se il file
    // unit e' stato comunque creato la riga si aggiorna a "✓ Installato".
    setWizardMsg("");
    try {
      const r = await installProjectService(projectId, { ...svc, env, description });
      if (r.ok) {
        setSuggestions(prev => prev.map(s => s.unit===svc.unit ? {...s,existing:true} : s));
        setWizardMsg(`✓ ${svc.unit} installato.`);
        setInstallingUnit(null);
      } else {
        setWizardMsg(`Errore durante l'installazione di ${svc.unit}.`);
        // Non chiudere la modale in caso di errore: l'utente puo' correggere e ritentare.
      }
    } catch (e) {
      setWizardMsg(`Errore di rete: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      // SEMPRE re-fetch dello stato reale: copre il caso in cui il backend
      // segnala errore ma il servizio e' stato comunque registrato (parziale),
      // o viceversa "ok" ma una verifica del gestore dice il contrario.
      // Cosi' la riga del wizard riflette la realta' del sistema.
      fetchServices();
    }
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
    void fetchNexusServices();
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

  return (
    <div style={{ display:"flex", flexDirection:"column", height:"100%", minHeight:0, overflow:"auto", position:"relative" }}>

      <NexusServicesSection
        tc={tc}
        nexusSvcs={nexusSvcs}
        nexusBusy={nexusBusy}
        nexusMsg={nexusMsg}
        nexusCollapsed={nexusCollapsed}
        setNexusCollapsed={setNexusCollapsed}
        fetchNexusServices={fetchNexusServices}
        handleNexusAction={handleNexusAction}
      />

      <ProjectServicesSection
        tc={tc}
        services={services}
        slug={slug}
        managerUnavailable={managerUnavailable}
        managerHint={managerHint}
        ports={ports}
        svcBusy={svcBusy}
        svcMsg={svcMsg}
        batchBusy={batchBusy}
        pendingCount={pendingCount}
        changedFiles={changedFiles}
        diagSentFor={diagSentFor}
        diagResult={diagResult}
        onSendToChat={onSendToChat}
        fetchServices={fetchServices}
        handleRestartAll={handleRestartAll}
        handleCleanupPorts={handleCleanupPorts}
        runWizard={runWizard}
        handleSvcAction={handleSvcAction}
        handleUninstall={handleUninstall}
        bumpLastRestart={bumpLastRestart}
        setDiagSentFor={setDiagSentFor}
        setDiagResult={setDiagResult}
      />

      <StaticSiteSection tc={tc} projectId={projectId} />

      <PortAllocationsSection
        tc={tc}
        projectId={projectId}
        portAllocations={portAllocations}
        setPortAllocations={setPortAllocations}
        showAddPort={showAddPort}
        setShowAddPort={setShowAddPort}
        newPortValue={newPortValue}
        setNewPortValue={setNewPortValue}
        newPortLabel={newPortLabel}
        setNewPortLabel={setNewPortLabel}
        portAllocMsg={portAllocMsg}
        setPortAllocMsg={setPortAllocMsg}
        fetchPortAllocations={fetchPortAllocations}
      />

      {wizardOpen && (
        <WizardOverlay
          tc={tc}
          suggestions={suggestions}
          wizardLoading={wizardLoading}
          wizardMsg={wizardMsg}
          setWizardOpen={setWizardOpen}
          setInstallingUnit={setInstallingUnit}
        />
      )}

      {/* Modali */}
      {installingUnit && (
        <WizardInstallModal
          svc={installingUnit}
          onInstall={handleInstall}
          onCancel={() => { setInstallingUnit(null); setWizardMsg(""); }}
          tc={tc}
          feedback={wizardMsg}
        />
      )}
    </div>
  );
}
