-- 0562_service_diagnoses_crash_dedup.sql
-- De-duplicazione dei crash del service_observer.
--
-- Problema (causa radice): il ramo crash di persist_diagnosis eseguiva un INSERT
-- cieco a ogni rilevazione strutturale (service_failed / port_not_listening /
-- restart_loop), mentre la guardia anti-spam e' in memoria (`st.last_crash_sig` in
-- service_observer.rs). A ogni restart di mcp-core lo stato in-memory si azzera,
-- mentre il crash resta 'open' nel DB: al ciclo successivo l'observer lo considera
-- "nuovo" e INSERISCE un'altra riga. Risultato: N record 'open' per lo STESSO
-- (unit, error_signature_hash) che gonfiano il pannello Problemi con crash
-- duplicati. La firma sig_hash(unit + prev_active_enter + reason) e' STABILE entro
-- la vita del servizio (error_signature_hash sempre valorizzato per crash).
--
-- Fix (stesso pattern di 0491 per le anomalie, regola L): un solo crash ATTIVO per
-- (project_id, unit, signal_kind, COALESCE(error_signature_hash, '')); i cicli
-- successivi AGGIORNANO quella riga (detail + updated_at + occurrences) invece di
-- inserirne di nuove. Realizzato con UPSERT (ON CONFLICT) in persist_diagnosis su
-- un indice univoco PARZIALE definito qui sotto.
--
-- L'auto-resolve (resolve_open_crashes / apply_run_reset) NON e' toccato: la
-- deduplica agisce solo sull'APERTURA. Le righe 'resolved' restano storico e non
-- sono coperte dal vincolo (piu' cicli di vita successivi per la stessa chiave).
--
-- signal_kind='policy_violation' NON e' toccato (semantica sicurezza, status extra
-- 'diagnosing'/'failed_remediation'). Le colonne occurrences/updated_at sono gia'
-- state aggiunte da 0491, resolved_at da 0367.
--
-- Idempotente.

-- ── Consolidamento dei duplicati crash esistenti ────────────────────────────
-- Per ogni (project_id, unit, signal_kind, error_signature_hash) con piu' righe
-- attive ('open'/'diagnosing'): si tiene la PIU' RECENTE (max created_at) come riga
-- canonica e si marcano le altre come 'resolved' (storico preservato, niente
-- DELETE). La canonica eredita occurrences = numero di righe accorpate e
-- updated_at = NOW(), cosi' il pannello scende subito a 1 problema per chiave.
-- signal_kind='crash' soltanto: anomaly ha gia' il proprio dedup (0491),
-- policy_violation ha ciclo di vita e stati propri.
--
-- Lo snapshot dei gruppi e' calcolato UNA volta (temp table); i due UPDATE
-- attingono allo stesso snapshot, quindi l'ordine fra loro e' irrilevante e la
-- canonica eredita il grp_size completo anche dopo che gli altri sono risolti.
CREATE TEMPORARY TABLE _dup_crash_0562 ON COMMIT DROP AS
SELECT
    id,
    ROW_NUMBER() OVER (
        PARTITION BY project_id, unit, signal_kind, COALESCE(error_signature_hash, '')
        ORDER BY created_at DESC, id DESC
    ) AS rn,
    COUNT(*) OVER (
        PARTITION BY project_id, unit, signal_kind, COALESCE(error_signature_hash, '')
    ) AS grp_size
FROM service_diagnoses
WHERE signal_kind = 'crash'
  AND status IN ('open', 'diagnosing');

-- La riga canonica (rn = 1) di ogni gruppo che aveva duplicati eredita il conteggio.
UPDATE service_diagnoses d
SET occurrences = GREATEST(d.occurrences, r.grp_size),
    updated_at  = NOW()
FROM _dup_crash_0562 r
WHERE d.id = r.id
  AND r.rn = 1
  AND r.grp_size > 1;

-- I duplicati (rn > 1) vengono marcati 'resolved' (storico preservato).
UPDATE service_diagnoses d
SET status      = 'resolved',
    resolved_at = NOW(),
    detail      = COALESCE(d.detail, '') || ' [deduplicato dalla migrazione 0562]'
FROM _dup_crash_0562 r
WHERE d.id = r.id
  AND r.rn > 1;

-- ── Indice univoco parziale: un solo crash ATTIVO per chiave ─────────────────
-- Abilita ON CONFLICT in persist_diagnosis e impedisce STRUTTURALMENTE i duplicati
-- futuri (resiliente ai restart, dove la guardia in-memory st.last_crash_sig
-- fallisce). COALESCE(error_signature_hash,'') perche' un indice univoco NULL-naive
-- considererebbe due NULL come distinti. Limitato a signal_kind='crash' e status
-- attivi: anomaly ha il suo indice (uniq_service_diagnoses_active_anomaly, 0491),
-- policy_violation resta fuori dal vincolo, e lo storico 'resolved' puo' accumulare
-- piu' righe per chiave.
CREATE UNIQUE INDEX IF NOT EXISTS uniq_service_diagnoses_active_crash
    ON service_diagnoses (project_id, unit, signal_kind, COALESCE(error_signature_hash, ''))
    WHERE signal_kind = 'crash' AND status IN ('open', 'diagnosing');
