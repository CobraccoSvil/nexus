-- 0658_remove_pending_steps_settings.sql
--
-- Rimuove i due setting `agent.closure.pending_steps_detection_enabled` /
-- `agent.closure.pending_steps_min_items` (mig 0430), diventati orfani.
--
-- Causa radice: il segnale "report con passi pendenti" che leggevano
-- (`_PENDING_STEPS_LABELS`, 62 etichette lessicali in 5 lingue +
-- `detect_pending_steps_report[_with]`) e' stato eliminato dal codice — non
-- spostato nel DB — perche' il presunto fallback che lo precedeva
-- (`closure_verdict`) non ha mai avuto un produttore nel motore nativo Rust
-- (ADR 0034: "closure_judge... resta una via complementare NON portata al
-- nativo"), quindi il ramo lessicale era l'UNICO decisore reale in
-- produzione, non una difesa in profondita' dietro un segnale strutturale
-- primario. Il sostituto e' `unfulfilled_signal_with` (declared_outcome via
-- task_complete, ADR 0034, poi structural_unfulfilled_signal), che non legge
-- nessuna delle due chiavi qui rimosse.
--
-- Senza questa migrazione le due righe restano nella tabella `settings` senza
-- alcun lettore nel codice (regola G: unica fonte di verita', niente
-- configurazione DB-driven orfana) e vengono classificate "morta" da
-- `scripts/audit-settings.sh --gate` (crates/xtask/src/audit_settings.rs,
-- `classify_db_key`), facendo fallire il gate `pnpm verify` contro la
-- baseline ratchet (`scripts/audit-settings-baseline.json`, morta=0 fisso).
--
-- Idempotente.

BEGIN;

DELETE FROM settings
 WHERE key IN (
    'agent.closure.pending_steps_detection_enabled',
    'agent.closure.pending_steps_min_items'
 );

COMMIT;
