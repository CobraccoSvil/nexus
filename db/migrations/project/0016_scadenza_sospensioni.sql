-- 0016_scadenza_sospensioni.sql
-- La sospensione di un run dichiara CHI l'ha prodotta e QUANDO smette di valere.
--
-- Causa radice (rilievo A4 del processo standard figure, ADR 0043; corretta nel
-- codice insieme a questa migrazione: nexus-agent-graph/src/decisions/
-- suspension_watch.rs + mcp-core/src/run_reaper.rs). Il gate duale sui passi
-- critici (W3, mig META 0677) sospende in HITL anche in Automatic: e' il punto
-- del requisito, perche' su un passo irreversibile con validatori discordi deve
-- decidere l'umano. Ma in Automatic/Continuous quell'umano non c'e', e nessuno
-- degli apparati che chiudono i run raccoglieva il caso:
--
--   - `run_reaper` esclude `awaiting_confirmation` PER CONTRATTO (mig 0392):
--     e' uno stato resumibile via checkpoint, e ucciderlo distruggerebbe lavoro;
--   - `ACTIVE_RUN_STATUSES` lo conta fra i run che OCCUPANO la sessione, quindi
--     ogni run successivo veniva rifiutato.
--
-- Conseguenza: un run notturno con due validatori in disaccordo restava appeso
-- per sempre e ingorgava la sessione. Al mattino non c'era un esito da leggere.
--
-- Perche' due colonne e non una. Sono due domande diverse, e conflaterle
-- toglierebbe la risposta alla seconda: `suspension_expires_at` dice QUANDO la
-- sospensione smette di valere (NULL = mai, comportamento storico);
-- `suspension_kind` dice CHI l'ha prodotta, ed e' cio' da cui si deriva il
-- `blocker` ADR 0034 dichiarato alla chiusura (`step_gate` -> safety,
-- `human_review` -> permission). Con la sola scadenza, un run chiuso non
-- saprebbe piu' dire perche' era fermo.
--
-- Vocabolario di `suspension_kind` (regola N, canonico inglese; punto unico
-- decisions::suspension_watch::SuspensionOrigin):
--   'human_review' = HITL ordinaria (tool mutativi in Confirm, approvazione del
--                    piano W2): nasce solo dove un umano e' al terminale;
--   'step_gate'    = gate duale sui passi critici: l'unico che sospende dove
--                    non c'e' nessuno ad ascoltare.
-- Niente CHECK sul valore: il vocabolario e' gia' chiuso a codice e una riga
-- con un kind ignoto si legge come `None` (mai degradata a un caso noto).
--
-- Le righe STORICHE restano a NULL su entrambe le colonne, ed e' corretto: una
-- sospensione nata prima di questa migrazione non ha una scadenza da far
-- maturare, e nessun apparato deve inventargliene una a posteriori. I run gia'
-- appesi si chiudono a mano o con un nuovo turno, come e' sempre stato.
--
-- KILL-SWITCH (reversibile a caldo, cache 60s, chiave nel DB META):
--   UPDATE settings SET value = '0'
--    WHERE key = 'orchestrator.suspension_watch_timeout_s';
-- A 0 la sorveglianza e' spenta: nessuna scadenza viene piu' scritta e le
-- colonne restano inerti. Le scadenze GIA' scritte continuano a maturare —
-- spegnere il flag ferma la produzione di nuove scadenze, non riapre i run gia'
-- chiusi.

ALTER TABLE agent_runs
    ADD COLUMN IF NOT EXISTS suspension_kind text,
    ADD COLUMN IF NOT EXISTS suspension_expires_at timestamp with time zone;

COMMENT ON COLUMN agent_runs.suspension_kind IS
    'Chi ha prodotto la sospensione: human_review (HITL ordinaria, umano al terminale) | step_gate (gate duale sui passi critici). Da qui si deriva il blocker ADR 0034 alla chiusura per scadenza. NULL = run mai sospeso o sospensione precedente alla mig project 0016.';

COMMENT ON COLUMN agent_runs.suspension_expires_at IS
    'Istante oltre il quale la sospensione non vale piu'' e il run viene chiuso blocked_needs_input dal run_reaper. NULL = nessuna scadenza (HITL in Confirm: l''utente e'' al terminale, oppure sorveglianza spenta). Punto unico del calcolo: decisions::suspension_watch::classify_suspension.';

-- Indice del REAPER: la sweep periodica chiede «quali sospensioni sono
-- maturate?» e deve restare O(scadute), non O(run del progetto). Parziale sulle
-- sole righe che una scadenza ce l'hanno davvero — che sono la minoranza (in
-- Confirm la colonna resta NULL), quindi l'indice non cresce coi run ordinari.
CREATE INDEX IF NOT EXISTS idx_agent_runs_suspension_expires
    ON agent_runs (suspension_expires_at)
    WHERE suspension_expires_at IS NOT NULL AND completed_at IS NULL;
