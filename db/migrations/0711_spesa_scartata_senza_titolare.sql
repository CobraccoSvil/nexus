-- 0711: la spesa SCARTATA si registra anche senza titolare contabile.
--
-- La mig 0701 ha dato una riga ai tentativi consumati-e-buttati, ma quella riga
-- si scrive solo se la richiesta porta un'identita' contabile utilizzabile
-- (`nexus_ledger::identity_from_metadata`: due UUID). Dove l'identita' non c'e'
-- il gateway usciva prima di scrivere e lasciava un WARN:
--
--   "gateway-ledger: tentativi scartati NON registrati, identita' assente o
--    non-UUID scarti=1"
--
-- osservato in diretta il 13/08/2026 su una risposta degenere di groq. Quella
-- chiamata e' stata pagata e non risulta da nessuna parte: nessuna query, nessun
-- report, nessuna vista. La domanda «un fornitore che costa poco ma fallisce
-- spesso conviene davvero?» — che e' una domanda su provider e modello, non su
-- chi ha chiamato — resta senza risposta proprio sulle righe che la
-- riguardano.
--
-- Le richieste SENZA identita' non sono un'anomalia: sono le chiamate di
-- sistema. `GwMetadata::default` (`NeuralCoreClient::generate_completion`) le
-- manda vuote; i percorsi interni (`rolling_summary`, `choices_extractor`)
-- mandano i segnaposto 'internal'/'system', che UUID non sono. Nessuna di quelle
-- chiamate ha un progetto a cui addebitare, e inventargliene uno sarebbe un
-- magic fallback (regola G).
--
-- Il difetto non era la guardia: era che questa tabella non sapeva
-- RAPPRESENTARE una spesa senza titolare. `user_id` e `project_id` erano NOT
-- NULL, quindi l'unica forma disponibile per "non lo so" era la riga assente —
-- che e' indistinguibile da "non e' successo niente" (regola Q: l'ignoto e' una
-- variante dichiarata, non un silenzio).
--
-- Cosa cambia:
--
--   1. le due colonne diventano NULLable: NULL = nessuna identita' contabile
--      nella richiesta. Il FK ON DELETE CASCADE resta invariato (un FK non
--      vincola le righe con chiave NULL);
--
--   2. l'identita' e' una COPPIA e resta atomica: mezza identita' non e'
--      rappresentabile. E' la stessa forma del tipo `nexus_ledger::Identity`,
--      che ha due campi entrambi valorizzati o non esiste;
--
--   3. l'assenza e' ammessa SOLO su `status = 'discarded'`. Non e' una
--      restrizione prudenziale, e' il confine contabile:
--
--      - una riga `discarded` non e' coperta da nessuna prenotazione (la
--        prenotazione di mcp-core viene finalizzata coi numeri del tentativo
--        RIUSCITO), quindi scriverla non puo' duplicare nulla;
--      - una riga `finalized` senza identita' potrebbe invece essere il caso
--        `IdentitaPersa` (`nexus_ledger::Declaration::audit`): identita' mandata
--        e non arrivata, con mcp-core che finalizzera' la propria prenotazione.
--        Due righe per la stessa chiamata, cioe' il doppio conteggio del
--        2026-07-27 in una forma nuova. Il gateway non puo' distinguere i due
--        casi dai soli metadata, quindi sul percorso di successo continua a NON
--        scrivere e a dichiararlo (`LedgerOutcome::NoIdentity`): quel contratto
--        non cambia.
--
-- Le QUOTE non cambiano comportamento e non serve un filtro nuovo:
-- `usage_for_quotas` aggancia le righe con `l.user_id = q.user_id` /
-- `l.project_id = q.project_id`, e in SQL un confronto con NULL non e' mai vero.
-- Una riga senza titolare e' invisibile a ogni scope per COSTRUZIONE, ed e' la
-- risposta giusta: nessuno scope l'ha consumata. Il test
-- `una_riga_senza_titolare_non_consuma_la_quota_di_nessuno` (nexus-ledger) lo
-- verifica invece di assumerlo.
--
-- Restano visibili dove servono: `ai_usage_analytics_view` (mig 0702) aggrega
-- per (provider, model, ora) senza nominare l'identita', quindi le colonne
-- `discarded_*` includono da subito anche la spesa di sistema.

ALTER TABLE ai_usage_ledger ALTER COLUMN user_id DROP NOT NULL;
ALTER TABLE ai_usage_ledger ALTER COLUMN project_id DROP NOT NULL;

ALTER TABLE ai_usage_ledger
    ADD CONSTRAINT ai_usage_ledger_identita_atomica_check
    CHECK ((user_id IS NULL) = (project_id IS NULL));

ALTER TABLE ai_usage_ledger
    ADD CONSTRAINT ai_usage_ledger_identita_solo_su_discarded_check
    CHECK (user_id IS NOT NULL OR status = 'discarded');

COMMENT ON COLUMN ai_usage_ledger.user_id IS
    'Utente a cui la chiamata e'' addebitata. NULL = la richiesta non portava identita'' contabile (chiamata di sistema); ammesso solo su status=discarded, e sempre insieme a project_id NULL. Vedi mig 0711.';

COMMENT ON COLUMN ai_usage_ledger.project_id IS
    'Progetto a cui la chiamata e'' addebitata (tenant_id sul wire del gateway). NULL = la richiesta non portava identita'' contabile; ammesso solo su status=discarded, e sempre insieme a user_id NULL. Vedi mig 0711.';
