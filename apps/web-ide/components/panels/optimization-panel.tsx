"use client";

import { useState, useCallback, useEffect, useRef } from "react";
import { useThemeColors } from "../../lib/theme";
import { useProjectStore, selectFilesRecent, useEventOfKind } from "../../lib/project-dispatcher/hooks";
import { selectQualityScan, selectFindingsUpdate } from "../../lib/project-dispatcher/store";
import {
  runQualityScan,
  getQualityFindings,
  markFindingFixed,
  markFindingFalsePositive,
  readProjectFileLines,
  scanProjectFile,
  submitDeepReview,
  getDeepReviewStatus,
  type QualityFinding,
  type QualityScanResult,
} from "../../lib/api-client";
import {
  OPT_API_BASE,
  retryHintForCategory,
  type OptimizationPanelProps,
  type FixQueueItem,
} from "./optimization/types";
import { OptimizationToolbar } from "./optimization/OptimizationToolbar";
import { CategorySidebar } from "./optimization/CategorySidebar";
import { FindingsList } from "./optimization/FindingsList";

export function OptimizationPanel({ projectId, onSendToChat, onAutoSendToChat, agentRunEndSignal }: OptimizationPanelProps) {
  const tc = useThemeColors();
  const storageKey = `nexus:optimization:${projectId}`;

  // Stato dipendenze infrastrutturali (Qdrant/embedder).
  // Polling ogni 30s: senza questo, dopo un restart di Nexus durante la stessa
  // sessione browser il banner "non disponibile" rimaneva stale all'infinito
  // perche l'effetto girava solo on-mount.
  const [depsOk, setDepsOk] = useState(true);
  useEffect(() => {
    let cancelled = false;
    const checkHealth = () => {
      fetch(`${OPT_API_BASE}/api/health`, { credentials: "include" })
        .then(r => r.json())
        .then(d => {
          if (cancelled) return;
          const comps = d.components as Record<string, boolean> | undefined;
          setDepsOk(comps?.qdrant !== false && comps?.embedder !== false);
        })
        .catch(() => { /* ignora — assume ok finche' un check riesce */ });
    };
    checkHealth();
    const interval = window.setInterval(checkHealth, 30_000);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, []);

  const [scanning, setScanning] = useState(false);
  const [scanResult, setScanResult] = useState<QualityScanResult | null>(() => {
    try { const s = sessionStorage.getItem(storageKey); return s ? JSON.parse(s).scanResult ?? null : null; } catch { return null; }
  });
  const [findings, setFindings] = useState<QualityFinding[]>(() => {
    try { const s = sessionStorage.getItem(storageKey); return s ? JSON.parse(s).findings ?? [] : []; } catch { return []; }
  });
  // Ref stabile ai findings correnti: usata negli useEffect per evitare stale closures
  // senza dover mettere `findings` nelle dipendenze (causerebbe loop infiniti).
  const findingsRef = useRef(findings);
  findingsRef.current = findings;
  const [activeCategory, setActiveCategory] = useState("all");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Coda file per "Fix Tutto": ogni click invia il prossimo file (persista in sessionStorage)
  const [fixQueue, setFixQueue] = useState<FixQueueItem[]>(() => {
    try { const s = sessionStorage.getItem(storageKey); return s ? JSON.parse(s).fixQueue ?? [] : []; } catch { return []; }
  });
  const [fixQueueIndex, setFixQueueIndex] = useState<number>(() => {
    try { const s = sessionStorage.getItem(storageKey); return s ? JSON.parse(s).fixQueueIndex ?? 0 : 0; } catch { return 0; }
  });

  // Auto-fix sequenziale
  const [autoFixEnabled, setAutoFixEnabled] = useState(false);
  const autoFixEnabledRef = useRef(false);
  autoFixEnabledRef.current = autoFixEnabled;

  // Selezione manuale findings
  const [selectedFindingIds, setSelectedFindingIds] = useState<Set<string>>(new Set());

  // IDs da marcare come fixed al prossimo agentRunEndSignal, indipendentemente da autoFixEnabled.
  // Usato per: fix singoli + ultimo file della coda auto-fix (che non ha iterazione successiva).
  const pendingMarkOnNextRunRef = useRef<QualityFinding[]>([]);

  // Contatore di retry per finding: evita loop infinito quando lo scanner produce un falso positivo
  // persistente (es. `await fetch()` classificato erroneamente come query DB). Dopo MAX_FIX_RETRIES
  // tentativi senza successo, il finding viene rimosso dalla coda automatica e lasciato all'utente.
  const MAX_FIX_RETRIES = 2;
  const fixRetryCountRef = useRef<Map<string, number>>(new Map());

  // Deep review (Gemini Batch API) state
  const [deepReviewJobId, setDeepReviewJobId] = useState<string | null>(() => {
    try { const s = sessionStorage.getItem(storageKey); return s ? JSON.parse(s).deepReviewJobId ?? null : null; } catch { return null; }
  });
  const [deepReviewState, setDeepReviewState] = useState<string | null>(null);
  const [deepReviewCompleted, setDeepReviewCompleted] = useState(0);
  const [deepReviewTotal, setDeepReviewTotal] = useState(0);
  const [deepReviewError, setDeepReviewError] = useState<string | null>(null);
  const [deepReviewSubmitting, setDeepReviewSubmitting] = useState(false);
  const deepReviewPollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  const stopDeepReviewPoll = () => {
    if (deepReviewPollRef.current) {
      clearInterval(deepReviewPollRef.current);
      deepReviewPollRef.current = null;
    }
  };

  const pollDeepReviewStatus = useCallback((jobId: string) => {
    stopDeepReviewPoll();
    deepReviewPollRef.current = setInterval(async () => {
      try {
        const status = await getDeepReviewStatus(projectId, jobId);
        setDeepReviewState(status.state);
        setDeepReviewCompleted(status.completed ?? 0);
        setDeepReviewTotal(status.total ?? 0);
        if (status.state === "JOB_STATE_SUCCEEDED") {
          stopDeepReviewPoll();
          if (status.results && status.results.length > 0) {
            // Merge results into findings list as QualityFinding objects
            const newFindings: QualityFinding[] = status.results.flatMap((r) =>
              (r.issues ?? []).map((issue, idx) => ({
                id: `batch-${r.path}-${idx}`,
                filePath: r.path,
                lineNumber: typeof issue.line === "number" ? issue.line : null,
                severity: (["high", "medium", "low"].includes(issue.severity) ? issue.severity : "low") as "high" | "medium" | "low",
                category: issue.category ?? "ai-review",
                title: issue.message ?? "AI Issue",
                detail: issue.suggestion ?? "",
                fixedAt: null,
              }))
            );
            setFindings((prev) => [...prev, ...newFindings]);
          }
        } else if (status.state === "JOB_STATE_FAILED" || status.state === "JOB_STATE_CANCELLED") {
          stopDeepReviewPoll();
          setDeepReviewError(`Batch job ${status.state.toLowerCase().replace("job_state_", "")}`);
        }
      } catch (err) {
        stopDeepReviewPoll();
        setDeepReviewError(String(err));
      }
    }, 5000);
  }, [projectId]);

  // Al mount: se c'è un jobId salvato in sessionStorage, verifica il suo stato e riavvia il polling
  const mountRecoveredRef = useRef(false);
  useEffect(() => {
    if (mountRecoveredRef.current || !projectId) return;
    mountRecoveredRef.current = true;
    if (!deepReviewJobId) return;
    // Verifica lo stato del job salvato
    getDeepReviewStatus(projectId, deepReviewJobId)
      .then((status) => {
        setDeepReviewState(status.state);
        setDeepReviewCompleted(status.completed ?? 0);
        setDeepReviewTotal(status.total ?? 0);
        if (status.state === "JOB_STATE_RUNNING") {
          pollDeepReviewStatus(deepReviewJobId);
        } else if (status.state === "JOB_STATE_SUCCEEDED" && status.results?.length) {
          const newFindings: QualityFinding[] = status.results.flatMap((r) =>
            (r.issues ?? []).map((issue, idx) => ({
              id: `batch-${r.path}-${idx}`,
              filePath: r.path,
              lineNumber: typeof issue.line === "number" ? issue.line : null,
              severity: (["high", "medium", "low"].includes(issue.severity) ? issue.severity : "low") as "high" | "medium" | "low",
              category: issue.category ?? "ai-review",
              title: issue.message ?? "AI Issue",
              detail: issue.suggestion ?? "",
              fixedAt: null,
            }))
          );
          setFindings((prev) => [...prev, ...newFindings]);
        }
      })
      .catch(() => {
        // Job non trovato o scaduto, pulisci
        setDeepReviewJobId(null);
        setDeepReviewState(null);
      });
  }, [projectId, deepReviewJobId, pollDeepReviewStatus]);

  useEffect(() => {
    return () => stopDeepReviewPoll();
  }, []);

  const handleDeepReview = async () => {
    setDeepReviewSubmitting(true);
    setDeepReviewError(null);
    setDeepReviewState(null);
    try {
      const res = await submitDeepReview(projectId);
      setDeepReviewJobId(res.jobId);
      setDeepReviewState("JOB_STATE_RUNNING");
      setDeepReviewTotal(res.fileCount ?? 0);
      setDeepReviewCompleted(0);
      pollDeepReviewStatus(res.jobId);
    } catch (err) {
      setDeepReviewError(String(err));
    } finally {
      setDeepReviewSubmitting(false);
    }
  };

  // Auto-fix: quando l'agente finisce un run, verifica se i finding sono stati effettivamente
  // risolti analizzando il file modificato. Se ancora presenti, invia un retry all'agente.
  // Se risolti, marca come fixed e procede con la coda.
  useEffect(() => {
    if (!agentRunEndSignal) return;
    const timer = setTimeout(async () => {
      // 1. Verifica post-fix per i findings in attesa
      if (pendingMarkOnNextRunRef.current.length > 0) {
        const pendingFindings = [...pendingMarkOnNextRunRef.current];
        pendingMarkOnNextRunRef.current = [];

        const resolved: QualityFinding[] = [];
        const stillPresent: QualityFinding[] = [];

        // Raggruppa per file per minimizzare le chiamate al backend
        const byFile = new Map<string, QualityFinding[]>();
        for (const f of pendingFindings) {
          const arr = byFile.get(f.filePath) ?? [];
          arr.push(f);
          byFile.set(f.filePath, arr);
        }

        for (const [filePath, fileFindings] of byFile.entries()) {
          try {
            const result = await scanProjectFile(projectId, filePath);
            const freshFindings = result.findings;
            for (const f of fileFindings) {
              // Un finding è ancora presente se esiste un match per title + category
              // con numero di riga entro ±10 (il refactoring può spostare le righe)
              const freshMatch = freshFindings.find(
                r =>
                  r.title === f.title &&
                  r.category === f.category &&
                  (r.lineNumber === null ||
                    f.lineNumber === null ||
                    Math.abs((r.lineNumber ?? 0) - (f.lineNumber ?? 0)) <= 10)
              );
              if (freshMatch) {
                // Aggiorna il detail con i valori freschi (es. numero righe aggiornato)
                stillPresent.push({ ...f, detail: freshMatch.detail });
              } else {
                resolved.push(f);
              }
            }
          } catch {
            // Se la scansione del file fallisce, considera risolto per non bloccare il flusso
            resolved.push(...fileFindings);
          }
        }

        // Marca come fixed solo i finding effettivamente risolti
        if (resolved.length > 0) {
          // Reset del contatore retry per i finding risolti (non servono più)
          for (const f of resolved) fixRetryCountRef.current.delete(f.id);
          await markBatchFixedRef.current(resolved.map(f => f.id));
          // NON chiamare fetchFindings qui: markBatchFixed fa già l'aggiornamento
          // ottimistico della UI; fetchFindings sovrascriverebbe lo stato ottimistico
          // con dati stale dal DB (le scritture fire-and-forget non hanno ancora commitato).
        }

        // Per i finding ancora presenti: invia retry all'agente con il detail aggiornato.
        // Il messaggio di retry e' context-aware: il suggerimento concreto dipende dal
        // category del finding (long-function != N+1 != parse_error). Senza questo, ogni
        // tipo riceveva il prompt long-function "estrai helper functions" — confondendo
        // l'agente quando il finding era N+1 o parse_error (bug 4 del test E2E).
        if (stillPresent.length > 0) {
          const sendFn = onAutoSendToChat ?? onSendToChat;
          if (sendFn) {
            const toRetry: QualityFinding[] = [];
            const exhausted: QualityFinding[] = [];
            for (const f of stillPresent) {
              const prev = fixRetryCountRef.current.get(f.id) ?? 0;
              if (prev >= MAX_FIX_RETRIES) {
                // Tentativi esauriti: non inviare altri messaggi automatici.
                // Il finding rimane visibile nella lista; l'utente lo gestirà manualmente.
                exhausted.push(f);
              } else {
                fixRetryCountRef.current.set(f.id, prev + 1);
                toRetry.push(f);
              }
            }
            for (const f of toRetry) {
              const suggestion = retryHintForCategory(f.category, f.title);
              const retriesLeft = MAX_FIX_RETRIES - (fixRetryCountRef.current.get(f.id) ?? 0);
              const retryMsg = [
                `⚠️ **Verifica post-fix fallita** per \`${f.filePath}\`:`,
                `Il problema **${f.title}** è ancora presente — ${f.detail}.`,
                ``,
                suggestion,
                `Usa \`read_file_lines\` per leggere lo stato attuale del file, poi \`edit_file\` per applicare le modifiche.`,
                `Se ritieni che il problema sia un FALSO POSITIVO (es. il pattern segnalato non è reale nel codice), spiega perché in chat e NON modificare il file: l'utente potrà marcarlo come "non rilevante".`,
                retriesLeft === 0 ? `⚠️ Ultimo tentativo automatico — se anche questo fallisce, il finding resterà in lista per revisione manuale.` : "",
              ].filter(Boolean).join("\n");
              sendFn(retryMsg);
            }
            // Rimette in pending solo i finding che hanno ancora tentativi disponibili
            pendingMarkOnNextRunRef.current = toRetry;
            // I finding con retry esauriti: rimuovi il contatore per non sporcare la mappa
            for (const f of exhausted) {
              fixRetryCountRef.current.delete(f.id);
            }
          } else {
            // Nessun canale di invio disponibile: segna come fixed comunque
            await markBatchFixedRef.current(stillPresent.map(f => f.id));
          }
        }
      }

      // Ri-scansione attiva: dopo ogni run agente, verifica se i findings HIGH
      // ancora aperti sono stati risolti dal lavoro dell'agente. Raggruppa per file
      // per minimizzare le chiamate, poi marca come fixed quelli scomparsi.
      // Questo copre il caso in cui l'agente modifica un file con findings attivi
      // senza che l'utente abbia cliccato "Fix" esplicitamente su quel finding.
      try {
        const activeHighFindings = findingsRef.current.filter(
          f => !f.fixedAt && (f.severity === "high" || f.severity === "medium")
        );
        if (activeHighFindings.length > 0) {
          const byFile = new Map<string, QualityFinding[]>();
          for (const f of activeHighFindings) {
            const arr = byFile.get(f.filePath) ?? [];
            arr.push(f);
            byFile.set(f.filePath, arr);
          }
          const autoResolved: string[] = [];
          for (const [filePath, fileFindings] of byFile.entries()) {
            try {
              const result = await scanProjectFile(projectId, filePath);
              const freshFindings = result.findings;
              for (const f of fileFindings) {
                const stillExists = freshFindings.find(
                  r =>
                    r.title === f.title &&
                    r.category === f.category &&
                    (r.lineNumber === null ||
                      f.lineNumber === null ||
                      Math.abs((r.lineNumber ?? 0) - (f.lineNumber ?? 0)) <= 10)
                );
                if (!stillExists) {
                  autoResolved.push(f.id);
                  fixRetryCountRef.current.delete(f.id);
                }
              }
            } catch {
              // Se la scansione del singolo file fallisce, salta — non bloccare gli altri
            }
          }
          if (autoResolved.length > 0) {
            await markBatchFixedRef.current(autoResolved);
          }
        }
      } catch {
        // Fallback: se la ri-scansione fallisce del tutto, refresh dal DB
      }
      // Refresh finale dal DB per sincronizzare eventuali cambiamenti non catturati
      setTimeout(() => { fetchFindingsRef.current(); }, 2000);

      // 2. Se l'auto-fix non è attivo, non proseguire con la coda
      if (!autoFixEnabledRef.current || (!onSendToChat && !onAutoSendToChat)) return;

      try {
        const s = sessionStorage.getItem(storageKey);
        if (!s) return;
        const data = JSON.parse(s);
        const queue: Array<{ filePath: string; findings: QualityFinding[] }> = data.fixQueue ?? [];
        const idx: number = data.fixQueueIndex ?? 0;

        // Il file appena processato è quello all'indice precedente (idx - 1).
        const justFixed = queue[idx - 1];
        if (justFixed?.findings?.length) {
          const ids = justFixed.findings.map((f: QualityFinding) => f.id);
          // L'aggiornamento ottimistico di markBatchFixed è sufficiente per il feedback visuale.
          // Non richiamiamo fetchFindings: le scritture DB sono fire-and-forget e potrebbero
          // non essere ancora committate, causando la ricomparsa del finding appena rimosso.
          await markBatchFixedRef.current(ids);
        }

        if (idx < queue.length) {
          await sendFileToFix(queue[idx], true);
          const nextIdx = idx + 1;
          setFixQueueIndex(nextIdx);
          sessionStorage.setItem(storageKey, JSON.stringify({ ...data, fixQueueIndex: nextIdx }));
          if (nextIdx >= queue.length) {
            // Ultimo file inviato: salva i finding completi per la verifica post-fix al prossimo signal
            const lastItem = queue[nextIdx - 1];
            if (lastItem?.findings?.length) {
              pendingMarkOnNextRunRef.current = lastItem.findings;
            }
            setAutoFixEnabled(false);
          }
        } else {
          setAutoFixEnabled(false);
        }
      } catch { /* ignore */ }
    }, 3000);
    return () => clearTimeout(timer);
  // dipendenze escluse intenzionalmente: onSendToChat/onAutoSendToChat/sendFileToFix sono callback da props lette in closure; storageKey/projectId derivano da projectId prop stabile
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agentRunEndSignal]);

  // Dispatcher: ascolta FileChanged dal dispatcher SSE per aggiornare il pannello
  // quando l'agente (o qualunque tool) modifica un file che ha findings attivi.
  // Questo complementa agentRunEndSignal: funziona anche quando il fix avviene
  // tramite un agente che non passa per onRunEnd della chat principale.
  const filesRecentChanged = useProjectStore(selectFilesRecent);
  const filesChangedCountRef = useRef(filesRecentChanged.length);
  useEffect(() => {
    if (filesRecentChanged.length === 0) return;
    if (filesChangedCountRef.current === filesRecentChanged.length) return;
    filesChangedCountRef.current = filesRecentChanged.length;

    // Verifica se il file cambiato ha findings pendenti
    const pendingFiles = new Set(pendingMarkOnNextRunRef.current.map(f => f.filePath));
    const changedPaths = filesRecentChanged.map(f => f.path);
    const hasRelevantChange = changedPaths.some(p => pendingFiles.has(p));

    if (hasRelevantChange && pendingMarkOnNextRunRef.current.length > 0) {
      // Delay per dare tempo al backend di scrivere il file
      const timer = setTimeout(async () => {
        const pending = [...pendingMarkOnNextRunRef.current];
        pendingMarkOnNextRunRef.current = [];

        const resolved: QualityFinding[] = [];
        const byFile = new Map<string, QualityFinding[]>();
        for (const f of pending) {
          const arr = byFile.get(f.filePath) ?? [];
          arr.push(f);
          byFile.set(f.filePath, arr);
        }

        for (const [filePath, fileFindings] of byFile.entries()) {
          try {
            const result = await scanProjectFile(projectId, filePath);
            for (const f of fileFindings) {
              const freshMatch = result.findings.find(
                r => r.title === f.title && r.category === f.category &&
                  (r.lineNumber === null || f.lineNumber === null ||
                    Math.abs((r.lineNumber ?? 0) - (f.lineNumber ?? 0)) <= 10)
              );
              if (!freshMatch) resolved.push(f);
            }
          } catch {
            resolved.push(...fileFindings);
          }
        }

        if (resolved.length > 0) {
          for (const f of resolved) fixRetryCountRef.current.delete(f.id);
          await markBatchFixedRef.current(resolved.map(f => f.id));
        }
      }, 2000);
      return () => clearTimeout(timer);
    }

    // Se non ci sono findings pendenti ma un file con findings attivi e' cambiato,
    // ri-scansiona quei file specifici per verificare se i findings sono stati risolti.
    const activeByFile = new Map<string, QualityFinding[]>();
    for (const f of findingsRef.current.filter(f => !f.fixedAt)) {
      const arr = activeByFile.get(f.filePath) ?? [];
      arr.push(f);
      activeByFile.set(f.filePath, arr);
    }
    const changedActiveFiles = changedPaths.filter(p => activeByFile.has(p));
    if (changedActiveFiles.length > 0) {
      const timer = setTimeout(async () => {
        const autoResolved: string[] = [];
        for (const filePath of changedActiveFiles) {
          const fileFindings = activeByFile.get(filePath) ?? [];
          try {
            const result = await scanProjectFile(projectId, filePath);
            for (const f of fileFindings) {
              const stillExists = result.findings.find(
                r =>
                  r.title === f.title &&
                  r.category === f.category &&
                  (r.lineNumber === null ||
                    f.lineNumber === null ||
                    Math.abs((r.lineNumber ?? 0) - (f.lineNumber ?? 0)) <= 10)
              );
              if (!stillExists) {
                autoResolved.push(f.id);
                fixRetryCountRef.current.delete(f.id);
              }
            }
          } catch {
            // Scansione fallita per questo file — salta
          }
        }
        if (autoResolved.length > 0) {
          await markBatchFixedRef.current(autoResolved);
        }
        // Refresh finale per sincronizzare
        setTimeout(() => { fetchFindingsRef.current(); }, 2000);
      }, 2000);
      return () => clearTimeout(timer);
    }
  }, [filesRecentChanged, projectId]);

  // Binding evento dispatcher `FindingsUpdated`: il backend emette dopo ogni
  // scan di quality (auto o on-demand) con il delta `resolved_ids` (lista
  // findings risolti rispetto allo stato precedente). Marchiamo in-place
  // SENZA ri-scansionare lato client, evitando flash banner e duplicate API.
  //
  // Sostituisce il vecchio setInterval(15000) che faceva polling locale dei
  // file con findings HIGH attivi — rumoroso e causa di flash banner periodici.
  useEventOfKind("FindingsUpdated", (env) => {
    const ids = env.payload.resolved_ids ?? [];
    if (ids.length === 0) return;
    void markBatchFixedRef.current(ids);
    // Refresh leggero per allineare anche eventuali findings nuovi non risolti
    setTimeout(() => { fetchFindingsRef.current(); }, 1000);
  }, [projectId]);

  // Auto-reset della coda dopo che tutti i file sono stati inviati (con delay per mostrare il messaggio)
  useEffect(() => {
    if (fixQueue.length === 0 || fixQueueIndex < fixQueue.length || autoFixEnabled) return;
    const t = setTimeout(() => {
      setFixQueue([]);
      setFixQueueIndex(0);
      try {
        const s = sessionStorage.getItem(storageKey);
        const data = s ? JSON.parse(s) : {};
        sessionStorage.setItem(storageKey, JSON.stringify({ ...data, fixQueue: [], fixQueueIndex: 0 }));
      } catch { /* ignore */ }
    }, 5000); // 5s per leggere "✓ Tutti i X file inviati", poi sparisce da solo
    return () => clearTimeout(t);
  }, [fixQueue.length, fixQueueIndex, autoFixEnabled, storageKey]);

  // Persiste findings, scanResult, coda fix e deepReviewJobId in sessionStorage quando cambiano
  useEffect(() => {
    try {
      sessionStorage.setItem(storageKey, JSON.stringify({ findings, scanResult, fixQueue, fixQueueIndex, deepReviewJobId }));
    } catch { /* ignore */ }
  }, [findings, scanResult, fixQueue, fixQueueIndex, deepReviewJobId, storageKey]);

  // Chiavi composite dei finding marcati come fixed nella sessione corrente.
  // Sopravvive ai re-scan (è un ref in memoria, non viene cancellato da fetchFindings).
  // Formato: "filePath|lineNumber|category|titlePrefix"
  const fixedInSessionRef = useRef<Set<string>>(new Set());
  const findingKey = (f: { filePath: string; lineNumber: number | null; category: string; title: string }) =>
    `${f.filePath}|${f.lineNumber ?? 0}|${f.category}|${f.title.slice(0, 40)}`;

  const fetchFindings = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await getQualityFindings(projectId, { limit: 5000 });
      // La scansione è la fonte di verità: i risultati vengono mostrati così come sono.
      // Se un finding è stato rilevato di nuovo, non è stato risolto → niente re-apply di fixedInSessionRef.
      setFindings(res.findings);
    } catch {
      setError("Errore nel caricamento dei findings");
    } finally {
      setLoading(false);
    }
  }, [projectId]);

  // Reset completo quando cambia progetto: evita di mostrare findings/stato
  // del progetto precedente. Il componente non si rimonta (stesso layout),
  // quindi gli inizializzatori useState non rieseguono — serve un effect esplicito.
  const prevProjectIdRef = useRef(projectId);
  useEffect(() => {
    if (prevProjectIdRef.current === projectId) return;
    prevProjectIdRef.current = projectId;
    // Reset di tutto lo stato in-memory
    setFindings([]);
    setScanResult(null);
    setFixQueue([]);
    setFixQueueIndex(0);
    setActiveCategory("all");
    setSelectedFindingIds(new Set());
    setAutoFixEnabled(false);
    fixedInSessionRef.current = new Set();
    pendingMarkOnNextRunRef.current = [];
    fixRetryCountRef.current = new Map();
    setDeepReviewJobId(null);
    setDeepReviewState(null);
    setDeepReviewError(null);
    stopDeepReviewPoll();
    mountRecoveredRef.current = false;
    // Carica i findings del nuovo progetto dal backend
    void fetchFindings();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId]);

  // Reazione a QualityScanProgress via dispatcher SSE: quando il backend
  // segnala "completed", ricarica i findings senza intervento manuale.
  const qualityScan = useProjectStore(selectQualityScan);
  useEffect(() => {
    if (!qualityScan) return;
    if (qualityScan.phase === "completed") {
      void fetchFindings();
    } else if (qualityScan.phase === "started") {
      setScanning(true);
    }
  }, [qualityScan, fetchFindings]);

  // Reazione a FindingsUpdated via dispatcher SSE: emesso dall'auto-scan per-file
  // (maybe_auto_scan_file) dopo ogni write/edit, inclusi i casi "file corretto"
  // (0 finding nuovi) che prima lasciavano i problemi risolti nel pannello.
  // Ricarica i findings cosi' la lista riflette lo stato reale senza scan manuale.
  const findingsUpdate = useProjectStore(selectFindingsUpdate);
  useEffect(() => {
    if (!findingsUpdate) return;
    void fetchFindings();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [findingsUpdate]);

  const handleScan = async () => {
    setScanning(true);
    setError(null);
    // Reset delle marcature in-memory: la scansione è la fonte di verità,
    // i finding rilevati di nuovo non sono stati risolti.
    fixedInSessionRef.current = new Set();
    pendingMarkOnNextRunRef.current = [];
    fixRetryCountRef.current = new Map(); // reset contatori retry ad ogni nuova scansione
    try {
      const result = await runQualityScan(projectId);
      setScanResult(result);
      await fetchFindings();
    } catch {
      setError("Errore durante la scansione");
    } finally {
      setScanning(false);
    }
  };

  const handleCategoryChange = (cat: string) => {
    setActiveCategory(cat);
    setSelectedFindingIds(new Set());
  };

  const handleFix = async (finding: QualityFinding) => {
    if (!onSendToChat) return;
    const loc = finding.lineNumber ? `:${finding.lineNumber}` : "";
    let codeBlock = "";
    if (finding.lineNumber && projectId) {
      try {
        const ctx = 40; // righe di contesto prima/dopo
        const start = Math.max(1, finding.lineNumber - ctx);
        const end = finding.lineNumber + ctx;
        const result = await readProjectFileLines(projectId, finding.filePath, start, end);
        codeBlock = `\n\n**Codice (righe ${result.startLine}–${result.endLine}):**\n\`\`\`\n${result.lines}\n\`\`\``;
      } catch {
        // Se non riesce, procede senza codice contestuale
      }
    }
    const noReadNote = codeBlock
      ? `📋 Il codice sotto è un riferimento. PRIMA di usare \`edit_file\`, leggi la sezione esatta con \`read_file_lines\` per verificare il contenuto attuale, poi usa \`edit_file\` con old_string di almeno 5 righe di contesto. Dopo ogni \`edit_file\` verifica il risultato — NON dichiarare successo senza aver ricevuto "modificato con successo".`
      : `Usa \`read_file_lines\` con offset/limit per leggere le sezioni da modificare. Dopo ogni \`edit_file\` verifica il risultato — NON dichiarare successo senza aver ricevuto "modificato con successo".`;
    const msg = `Fix questo problema nel file \`${finding.filePath}${loc}\`:\n**${finding.title}** — ${finding.detail}\n\n${noReadNote}${codeBlock}`;
    // Traccia questo finding: verrà marcato fixed al prossimo agentRunEndSignal
    pendingMarkOnNextRunRef.current = [finding];
    onSendToChat(msg);
  };

  // Ref stabile a fetchFindings per usarla in useEffect senza stale closure
  const fetchFindingsRef = useRef(fetchFindings);
  fetchFindingsRef.current = fetchFindings;

  // Ref stabile per marcare un insieme di findings come fixed (ottimistica).
  // Usata nell'auto-fix useEffect dopo ogni run dell'agente e in handleFixNext.
  const markBatchFixedRef = useRef(async (_ids: string[]) => {});
  markBatchFixedRef.current = async (ids: string[]) => {
    const now = new Date().toISOString();
    // Aggiorna UI subito (ottimistica) e salva le chiavi composite per sopravvivere ai re-scan
    setFindings(prev => {
      const idSet = new Set(ids);
      prev.filter(f => idSet.has(f.id)).forEach(f => fixedInSessionRef.current.add(findingKey(f)));
      return prev.map(f => idSet.has(f.id) ? { ...f, fixedAt: now } : f);
    });
    // Persiste nel DB in background (fire-and-forget, ignora errori)
    for (const id of ids) {
      markFindingFixed(projectId, id).catch(() => {});
    }
  };

  // Costruisce la coda da una lista arbitraria di findings e avvia il primo
  const startFixQueue = (targetFindings: QualityFinding[], autoFix = false) => {
    if (!onSendToChat) return;
    if (targetFindings.length === 0) return;

    // Raggruppa per file, ordinati per numero di problemi decrescente
    const byFile = new Map<string, QualityFinding[]>();
    for (const f of targetFindings) {
      const arr = byFile.get(f.filePath) ?? [];
      arr.push(f);
      byFile.set(f.filePath, arr);
    }
    // Limita a max 25 findings per call: file grandi vengono spezzati in chunk.
    // Evita messaggi >50KB che eccedono il context window di Codestral (32K tok).
    const MAX_PER_CALL = 25;
    const queue = Array.from(byFile.entries())
      .sort((a, b) => b[1].length - a[1].length)
      .flatMap(([filePath, fileFindings]) => {
        if (fileFindings.length <= MAX_PER_CALL) {
          return [{ filePath, findings: fileFindings }];
        }
        // Spezza in chunk da MAX_PER_CALL
        const chunks = [];
        for (let i = 0; i < fileFindings.length; i += MAX_PER_CALL) {
          chunks.push({ filePath, findings: fileFindings.slice(i, i + MAX_PER_CALL) });
        }
        return chunks;
      });

    if (autoFix) setAutoFixEnabled(true);
    setFixQueue(queue);
    setFixQueueIndex(1); // il primo è inviato ora, il prossimo sarà l'indice 1
    setSelectedFindingIds(new Set()); // azzera selezione: evita accumulo con il prossimo giro
    // Persiste subito in sessionStorage (non aspetta l'useEffect)
    try {
      const s = sessionStorage.getItem(storageKey);
      const data = s ? JSON.parse(s) : {};
      sessionStorage.setItem(storageKey, JSON.stringify({ ...data, fixQueue: queue, fixQueueIndex: 1 }));
    } catch { /* ignore */ }
    sendFileToFix(queue[0], autoFix);
  };

  // Invia il prossimo file in coda e marca il precedente come fixed
  const handleFixNext = () => {
    if (!onSendToChat || fixQueueIndex >= fixQueue.length) return;
    // Marca il file appena fixato come fixed (ottimistico)
    const justFixed = fixQueue[fixQueueIndex - 1];
    if (justFixed?.findings?.length) {
      const ids = justFixed.findings.map(f => f.id);
      markBatchFixedRef.current(ids).catch(() => {});
    }
    const nextIndex = fixQueueIndex + 1;
    sendFileToFix(fixQueue[fixQueueIndex]);
    setFixQueueIndex(nextIndex);
    try {
      const s = sessionStorage.getItem(storageKey);
      const data = s ? JSON.parse(s) : {};
      sessionStorage.setItem(storageKey, JSON.stringify({ ...data, fixQueueIndex: nextIndex }));
    } catch { /* ignore */ }
  };

  const sendFileToFix = async (item: { filePath: string; findings: QualityFinding[] }, autoSend = false) => {
    const sendFn = autoSend && onAutoSendToChat ? onAutoSendToChat : onSendToChat;
    if (!sendFn) return;

    // Raccoglie tutti i numeri di riga unici, con contesto
    const lineNumbers = item.findings
      .map(f => f.lineNumber)
      .filter((n): n is number => typeof n === "number");

    // Costruisce sezioni di codice per ogni cluster di righe
    const codeBlocks: string[] = [];
    if (lineNumbers.length > 0 && projectId) {
      // Ordina e raggruppa righe vicine (entro 80 righe) in un unico blocco
      const sorted = [...new Set(lineNumbers)].sort((a, b) => a - b);
      const clusters: Array<[number, number]> = [];
      let clusterStart = sorted[0];
      let clusterEnd = sorted[0];
      for (let i = 1; i < sorted.length; i++) {
        if (sorted[i] - clusterEnd <= 80) {
          clusterEnd = sorted[i];
        } else {
          clusters.push([clusterStart, clusterEnd]);
          clusterStart = sorted[i];
          clusterEnd = sorted[i];
        }
      }
      clusters.push([clusterStart, clusterEnd]);

      for (const [start, end] of clusters) {
        try {
          const ctx = 20;
          const result = await readProjectFileLines(
            projectId,
            item.filePath,
            Math.max(1, start - ctx),
            end + ctx
          );
          codeBlocks.push(`**Righe ${result.startLine}–${result.endLine}:**\n\`\`\`\n${result.lines}\n\`\`\``);
        } catch {
          // ignora errori di lettura
        }
      }
    }

    const list = item.findings.map(f => {
      const loc = f.lineNumber ? `:${f.lineNumber}` : "";
      return `- \`${item.filePath}${loc}\`: [${f.category}] ${f.title} — ${f.detail}`;
    }).join("\n");

    const fileName = item.filePath.split("/").pop() ?? item.filePath;
    const codeSection = codeBlocks.length > 0
      ? `\n\n**Codice contestuale estratto da \`${fileName}\`:**\n\n${codeBlocks.join("\n\n")}`
      : "";

    const noReadInstruction = codeBlocks.length > 0
      ? `📋 Il codice contestuale è incluso sotto come riferimento. PRIMA di usare \`edit_file\`, leggi la sezione esatta con \`read_file_lines\` per verificare che l'old_string corrisponda al contenuto attuale (il file potrebbe essere stato modificato da edit precedenti).`
      : `Usa \`read_file_lines\` con offset/limit per leggere le sezioni da modificare prima di usare \`edit_file\`.`;

    // Istruzione generale per edit_file (sempre inclusa)
    const editFileRules = `
🔧 REGOLE OBBLIGATORIE per \`edit_file\`:
1. LEGGI PRIMA: usa \`read_file_lines\` per leggere la sezione esatta del file (intorno alla riga indicata) PRIMA di ogni \`edit_file\`. Il codice incluso nel messaggio è un riferimento, non garantisce lo stato attuale del file.
2. \`old_string\` DEVE essere unico nel file — includi almeno 5 righe di contesto sopra e sotto. MAI usare pattern generici (es. "useRef<any>(null)" da solo — può apparire più volte).
3. Dopo ogni \`edit_file\` verifica il risultato: se "[Errore: old_string non trovato]" rileggere con \`read_file_lines\` e correggere. Se "[Errore: old_string trovato X volte]" usare un \`old_string\` più specifico.
4. NON dichiarare mai di aver completato un fix senza aver ricevuto "File modificato con successo" dall'tool.`;

    const maxSeverity = item.findings.some(f => f.severity === "high")
      ? "HIGH"
      : item.findings.some(f => f.severity === "medium") ? "MEDIUM" : "LOW";

    // Istruzioni specifiche per categoria
    const categories = new Set(item.findings.map(f => f.category));
    const categoryInstructions: string[] = [];
    if (categories.has("typing")) {
      categoryInstructions.push(
        `🔴 REGOLA CRITICA per i problemi di tipo TypeScript:
Il detector segnala qualsiasi riga che contenga ": any", "as any" oppure "<any>" (come in Array<any>, Promise<any>, Record<string, any>).
Devi eliminare TUTTE le occorrenze di "any" da quelle righe — non basta cambiare una sola occorrenza.
Sostituisci con tipi specifici: usa "unknown" se il tipo è davvero sconosciuto, oppure interfacce, union types, generics propri. VIETATO lasciare "any" in qualsiasi forma (": any", "as any", "<any>") sulla stessa riga.`
      );
    }
    if (categories.has("imports")) {
      categoryInstructions.push(
        `🔴 REGOLA per import non usati: rimuovi completamente la riga di import, non commentarla.`
      );
    }
    const extraInstructions = categoryInstructions.length > 0
      ? `\n\n${categoryInstructions.join("\n\n")}`
      : "";

    sendFn(
      `Correggi questi problemi ${maxSeverity} severity nel file \`${item.filePath}\`.\n` +
      `${noReadInstruction}${editFileRules}${extraInstructions}\n\n${list}${codeSection}`
    );
  };

  const handleMarkFixed = async (finding: QualityFinding) => {
    try {
      await markFindingFixed(projectId, finding.id);
      setFindings(prev => prev.map(f => f.id === finding.id ? { ...f, fixedAt: new Date().toISOString() } : f));
    } catch { /* ignore */ }
  };

  const markFalsePositive = async (findingId: string, ruleKey?: string) => {
    try {
      await markFindingFalsePositive(findingId, undefined, ruleKey);
      setFindings(prev => prev.filter(f => f.id !== findingId));
    } catch (e) {
      console.error('Failed to mark false positive', e);
    }
  };

  const allActiveFindings = findings.filter(f => !f.fixedAt);
  const visibleFindings = activeCategory === "all"
    ? allActiveFindings
    : allActiveFindings.filter(f => f.category === activeCategory);
  const highCount = allActiveFindings.filter(f => f.severity === "high").length;
  const mediumCount = allActiveFindings.filter(f => f.severity === "medium").length;
  const lowCount = allActiveFindings.filter(f => f.severity === "low").length;

  const catCounts: Record<string, number> = {};
  for (const f of allActiveFindings) {
    catCounts[f.category] = (catCounts[f.category] ?? 0) + 1;
  }

  // Findings selezionati (dall'intero set, non solo visibili — preserva la selezione tra categorie)
  const selectedFindings = allActiveFindings.filter(f => selectedFindingIds.has(f.id));
  // HIGH nella categoria/vista corrente (per i pulsanti "Fix categoria")
  const visibleHighFindings = visibleFindings.filter(f => f.severity === "high");

  const toggleFindingSelection = (id: string) => {
    setSelectedFindingIds(prev => {
      const next = new Set(prev);
      if (next.has(id)) { next.delete(id); } else { next.add(id); }
      return next;
    });
  };

  const toggleSelectAllVisible = () => {
    const allVisibleSelected = visibleFindings.length > 0 && visibleFindings.every(f => selectedFindingIds.has(f.id));
    if (allVisibleSelected) {
      // Deseleziona solo i visibili, mantieni eventuali selezioni in altre categorie
      setSelectedFindingIds(prev => {
        const next = new Set(prev);
        visibleFindings.forEach(f => next.delete(f.id));
        return next;
      });
    } else {
      setSelectedFindingIds(prev => {
        const next = new Set(prev);
        visibleFindings.forEach(f => next.add(f.id));
        return next;
      });
    }
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", height: "100%", minHeight: 0 }}>
      {/* Toolbar */}
      <OptimizationToolbar
        tc={tc}
        scanning={scanning}
        depsOk={depsOk}
        onSendToChat={onSendToChat}
        scanResult={scanResult}
        allActiveFindings={allActiveFindings}
        highCount={highCount}
        mediumCount={mediumCount}
        lowCount={lowCount}
        fixQueue={fixQueue}
        fixQueueIndex={fixQueueIndex}
        autoFixEnabled={autoFixEnabled}
        setAutoFixEnabled={setAutoFixEnabled}
        storageKey={storageKey}
        setFixQueue={setFixQueue}
        setFixQueueIndex={setFixQueueIndex}
        handleScan={handleScan}
        startFixQueue={startFixQueue}
        handleFixNext={handleFixNext}
        deepReviewSubmitting={deepReviewSubmitting}
        deepReviewState={deepReviewState}
        deepReviewCompleted={deepReviewCompleted}
        deepReviewTotal={deepReviewTotal}
        deepReviewError={deepReviewError}
        deepReviewJobId={deepReviewJobId}
        handleDeepReview={handleDeepReview}
        stopDeepReviewPoll={stopDeepReviewPoll}
        setDeepReviewJobId={setDeepReviewJobId}
        setDeepReviewState={setDeepReviewState}
        setDeepReviewError={setDeepReviewError}
        setDeepReviewCompleted={setDeepReviewCompleted}
        setDeepReviewTotal={setDeepReviewTotal}
        pollDeepReviewStatus={pollDeepReviewStatus}
      />
      {error && (
        <div style={{ padding: "6px 10px", color: tc.error, fontSize: 12, flexShrink: 0 }}>{error}</div>
      )}

      {!scanResult ? (
        <div style={{ padding: 16, color: tc.textMuted, fontSize: 13 }}>
          {scanning ? "Scansione in corso…" : "Clicca \"Scansiona\" per analizzare la qualità del progetto."}
        </div>
      ) : (
        <div style={{ display: "flex", flex: 1, minHeight: 0 }}>
          {/* Category sidebar */}
          <CategorySidebar
            tc={tc}
            activeCategory={activeCategory}
            allActiveFindingsCount={allActiveFindings.length}
            catCounts={catCounts}
            handleCategoryChange={handleCategoryChange}
          />

          {/* Findings list */}
          <FindingsList
            tc={tc}
            loading={loading}
            visibleFindings={visibleFindings}
            selectedFindingIds={selectedFindingIds}
            selectedFindings={selectedFindings}
            visibleHighFindings={visibleHighFindings}
            activeCategory={activeCategory}
            onSendToChat={onSendToChat}
            fixQueue={fixQueue}
            toggleSelectAllVisible={toggleSelectAllVisible}
            startFixQueue={startFixQueue}
            toggleFindingSelection={toggleFindingSelection}
            handleFix={handleFix}
            handleMarkFixed={handleMarkFixed}
            markFalsePositive={markFalsePositive}
          />
        </div>
      )}
    </div>
  );
}
