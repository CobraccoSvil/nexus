-- 0367_service_diagnoses_resolved_at.sql
-- Auto-resolve delle anomalie del service_observer.
--
-- Problema: le diagnosi signal_kind='anomaly' (cpu/rss/restart/error_rate)
-- venivano scritte con status='open' alla transizione healthy->anomaly, ma non
-- venivano MAI richiuse quando il servizio rientrava sotto soglia. Il pannello
-- "Problemi" legge `WHERE status='open'`, quindi mostrava anomalie gia' rientrate
-- come problemi "fantasma" a tempo indeterminato (nessun worker di cleanup).
--
-- Fix: il service_observer ora chiude (status='resolved') le anomalie aperte la
-- cui metrica non supera piu' la soglia (vedi resolve_stale_anomalies in
-- service_observer.rs). Questa colonna allinea service_diagnoses al pattern gia'
-- usato da project_runtime_issues (status + resolved_at) per la tracciabilita'.
-- Idempotente.

ALTER TABLE service_diagnoses
    ADD COLUMN IF NOT EXISTS resolved_at TIMESTAMPTZ;

-- Backfill una-tantum: le anomalie storiche gia' 'open' restano gestite a runtime
-- dall'observer (le richiude al primo ciclo in cui la metrica e' sotto soglia).
-- Nessun UPDATE massivo qui: lo stato di salute reale lo conosce solo l'observer.
