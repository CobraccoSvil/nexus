-- 0491_service_diagnoses_anomaly_dedup.sql
-- De-duplicazione delle anomalie del service_observer.
--
-- Problema (causa radice): persist_diagnosis eseguiva un INSERT cieco a ogni
-- transizione healthy->anomaly, ma la guardia che evita la ri-emissione e' in
-- memoria (HashSet `active_anomalies` in service_observer.rs). A ogni restart di
-- mcp-core lo stato in-memory si azzera, mentre l'anomalia resta 'open' nel DB:
-- al ciclo successivo l'observer la considera "nuova" e INSERISCE un'altra riga.
-- Risultato: N record open per lo STESSO (unit, signal_kind, metric) che gonfiano
-- il pannello Problemi (caso reale: beauty-book-frontend.service / error_rate ->
-- 12 righe open con value 60,177,193,315,518,689,831,833,...).
--
-- Fix: una sola anomalia ATTIVA per (project_id, unit, signal_kind, metric); i
-- tick successivi AGGIORNANO quella riga (value + updated_at + occurrences)
-- invece di inserirne di nuove. Realizzato con UPSERT (ON CONFLICT) in
-- persist_diagnosis su un indice univoco PARZIALE definito qui sotto.
--
-- L'auto-resolve (resolve_stale_anomalies / resolve_anomalies_for_absent_units)
-- NON e' toccato: la deduplica agisce solo sull'APERTURA. Le righe 'resolved'
-- restano storico e non sono coperte dal vincolo (puo' esistere piu' di una
-- 'resolved' per la stessa chiave: cicli di vita successivi).
--
-- Idempotente.

-- ── Nuove colonne: tracciamento aggiornamenti su anomalia attiva ─────────────
ALTER TABLE service_diagnoses
    ADD COLUMN IF NOT EXISTS updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW();

-- Quante volte la stessa anomalia attiva e' stata ri-rilevata (1 = prima
-- emissione). Permette di mostrare "persiste da N tick" senza N righe.
ALTER TABLE service_diagnoses
    ADD COLUMN IF NOT EXISTS occurrences INTEGER NOT NULL DEFAULT 1;

-- ── Consolidamento dei duplicati esistenti ──────────────────────────────────
-- Per ogni (project_id, unit, signal_kind, metric) con piu' righe attive
-- ('open'/'diagnosing'): si tiene la PIU' RECENTE (max created_at) come riga
-- canonica e si marcano le altre come 'resolved' (storico preservato, niente
-- DELETE). La canonica eredita occurrences = numero di righe accorpate e
-- updated_at = NOW(), cosi' il pannello scende subito a 1 problema per chiave.
-- signal_kind='anomaly' soltanto: crash/build_error hanno ciclo di vita proprio.
--
-- Lo snapshot dei gruppi e' calcolato UNA volta (CTE `ranked`); i due UPDATE
-- attingono allo stesso snapshot, quindi l'ordine fra loro e' irrilevante e la
-- canonica eredita il grp_size completo anche dopo che gli altri sono risolti.
CREATE TEMPORARY TABLE _dup_0491 ON COMMIT DROP AS
SELECT
    id,
    ROW_NUMBER() OVER (
        PARTITION BY project_id, unit, signal_kind, COALESCE(metric, '')
        ORDER BY created_at DESC, id DESC
    ) AS rn,
    COUNT(*) OVER (
        PARTITION BY project_id, unit, signal_kind, COALESCE(metric, '')
    ) AS grp_size
FROM service_diagnoses
WHERE signal_kind = 'anomaly'
  AND status IN ('open', 'diagnosing');

-- La riga canonica (rn = 1) di ogni gruppo che aveva duplicati eredita il conteggio.
UPDATE service_diagnoses d
SET occurrences = GREATEST(d.occurrences, r.grp_size),
    updated_at  = NOW()
FROM _dup_0491 r
WHERE d.id = r.id
  AND r.rn = 1
  AND r.grp_size > 1;

-- I duplicati (rn > 1) vengono marcati 'resolved' (storico preservato).
UPDATE service_diagnoses d
SET status      = 'resolved',
    resolved_at = NOW(),
    detail      = COALESCE(d.detail, '') || ' [deduplicato dalla migrazione 0491]'
FROM _dup_0491 r
WHERE d.id = r.id
  AND r.rn > 1;

-- ── Indice univoco parziale: una sola anomalia ATTIVA per chiave ─────────────
-- Abilita ON CONFLICT in persist_diagnosis e impedisce STRUTTURALMENTE i
-- duplicati futuri (resiliente ai restart, dove la guardia in-memory fallisce).
-- COALESCE(metric,'') perche' un indice univoco NULL-naive considererebbe due
-- NULL come distinti. Limitato a signal_kind='anomaly' e status attivi: crash e
-- build_error restano eventi distinti non vincolati, e lo storico 'resolved' puo'
-- accumulare piu' righe per chiave.
CREATE UNIQUE INDEX IF NOT EXISTS uniq_service_diagnoses_active_anomaly
    ON service_diagnoses (project_id, unit, COALESCE(metric, ''))
    WHERE signal_kind = 'anomaly' AND status IN ('open', 'diagnosing');
