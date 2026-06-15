-- 0429_aggressive_context_reduction.sql
-- Hardening qualita' agentico (2026-06-15): osservato sulla UI Nexus contesto al
-- 189% / 100% nei run reali, causa di:
--   - tool_use emesso come testo XML (modello cade a output non strutturato
--     quando il prompt e' troppo grande);
--   - cascata fallback su provider HEALTHY (output non riconosciuto = fallito);
--   - mescolamento di task vecchi e nuovi nel messaggio finale;
--   - "Nexus non arriva mai a una conclusione applicata" sui task complessi.
--
-- Le funzionalita' di riduzione token sono GIA' applicate nel codice (verified
-- 2026-06-15): context_offload.py legge rag_offload.*, nodes/__init__.py applica
-- max_context_ratio, compress_phase_*, rolling_summary, forced_rag. Il problema
-- non e' "non scattano", e' "scattano troppo tardi": compact solo all'80%, RAG
-- forzato solo al 40%, items < 2KB mai offloadati.
--
-- Questa migrazione abbassa i threshold per anticipare la riduzione del context:
-- compact a 60% (invece di 80%), max_context_ratio 55% (invece di 70%), RAG
-- offload a 30% (invece di 40%), items > 800 char offloadati (invece di 2000),
-- rolling summary su finestre piu' strette (3 turni invece di 5), compressione
-- per fase piu' aggressiva e che parte alla 3a iterazione (invece di 5a). Anche
-- discovery tool ridotti (12 schemi iniettati invece di 20).
--
-- Idempotente (UPDATE su chiavi esistenti). Cache DB-driven 60s nel codice:
-- nessun restart necessario. Valori vecchi documentati in commento per rollback.

BEGIN;

-- TRIGGER del compact: scatta al 60% del context window (era 80%, troppo tardi
-- quando il task era gia' ingombro)
UPDATE settings SET value = '0.60', updated_at = NOW()
 WHERE key = 'agent.context.auto_compact_ratio' AND value <> '0.60';
-- vecchio: 0.80

-- LIMITE FINALE prima dell'invio al modello: 55% (era 70%). Lascia margine al
-- modello per emettere tool_use strutturati anche su task lunghi.
UPDATE settings SET value = '0.55', updated_at = NOW()
 WHERE key = 'agent.context.max_context_ratio' AND value <> '0.55';
-- vecchio: 0.70

-- TRIGGER del RAG offload (forced): scatta al 30% (era 40%). Spinge prima i
-- tool_results pesanti in Qdrant invece di tenerli inline.
UPDATE settings SET value = '0.30', updated_at = NOW()
 WHERE key = 'agent.context.forced_rag_threshold_ratio' AND value <> '0.30';
-- vecchio: 0.40

-- SOGLIA per RAG offload di un singolo item: > 800 caratteri (era 2000). Tagli
-- mediamente lunghi di file/output finiscono in vector store invece di inline.
UPDATE settings SET value = '800', updated_at = NOW()
 WHERE key = 'agent.context.rag_offload.min_chars' AND value <> '800';
-- vecchio: 2000

-- ROLLING SUMMARY: finestra 3 turni (era 5), tengo solo 2 turni recenti (era 3).
-- I turni precedenti finiscono nel summary compresso.
UPDATE settings SET value = '3', updated_at = NOW()
 WHERE key = 'agent.context.rolling_window_turns' AND value <> '3';
-- vecchio: 5
UPDATE settings SET value = '2', updated_at = NOW()
 WHERE key = 'agent.context.rolling_keep_recent_turns' AND value <> '2';
-- vecchio: 3

-- COMPRESSIONE PER FASE: piu' aggressiva su tutti i parametri (parto prima,
-- comprimo di piu', tengo meno turni recenti).
UPDATE settings SET value = '3', updated_at = NOW()
 WHERE key = 'agent.context.compress_start_iter' AND value <> '3';
-- vecchio: 5

UPDATE settings SET value = '3,7,15,30', updated_at = NOW()
 WHERE key = 'agent.context.compress_phase_boundaries' AND value <> '3,7,15,30';
-- vecchio: 5,10,20,50 — fasi piu' strette: la fase "pesante" inizia prima

UPDATE settings SET value = '5,3,2,1', updated_at = NOW()
 WHERE key = 'agent.context.compress_phase_keep_recent' AND value <> '5,3,2,1';
-- vecchio: 8,5,3,2 — tieni meno turni recenti in ogni fase

UPDATE settings SET value = '1200,600,300,100', updated_at = NOW()
 WHERE key = 'agent.context.compress_phase_max_chars' AND value <> '1200,600,300,100';
-- vecchio: 2000,1000,500,150 — meno char nel summary per fase

-- TOOL DISCOVERY: meno schemi iniettati alla partenza (12 invece di 20). I tool
-- non whitelist si raggiungono comunque via nexus_mcp_tool_search (gia' core).
UPDATE settings SET value = '12', updated_at = NOW()
 WHERE key = 'agent.tools.discovery_max_injected' AND value <> '12';
-- vecchio: 20

-- CAP PREDITTIVO: anticipa il limite (40% invece di 50%): la stima predittiva
-- (prima di chiamare il modello) scarta materiale prima che esploda.
UPDATE settings SET value = '0.40', updated_at = NOW()
 WHERE key = 'agent.context.predictive_cap_ratio' AND value <> '0.40';
-- vecchio: 0.5

COMMIT;
