-- 0722_slot_base_capability.sql (F3-02, fase 3 lotto 2)
--
-- La slot-matrix converge sul SERVIZIO UNICO di selezione (`select_model`,
-- regola L): il requisito derivato da una chiave slot diventa una
-- `ModelRequest`, e `select_model` filtra su UNA capability (colonna dedicata
-- o jsonb), non su un array con soglia di match al 50% come lo scoring del
-- promoter. La colonna `base_capability` e' la traduzione del requisito nel
-- vocabolario del servizio: la capability che DEFINISCE il compito (la stessa
-- nozione di `base_capability` in `nexus_intent_routing_requirements`).
--
-- SEED derivato dalle righe esistenti (mig 0357): 'code' dove presente (e' la
-- base di ogni riga che la dichiara), altrimenti il primo elemento, NULL per
-- gli array vuoti (es. write/docs: nessuna capability richiesta).
--
-- NOTA sul resto del requisito (deviazione dichiarata dal design F3-02):
-- `cost_direction` e le altre `required_capabilities` muoiono CON LA DELEGA,
-- non con questa migrazione. Il rollout e' a flag (`routing.slots_via_select_model`,
-- mig 0724) con rollback = flip a OFF senza deploy: finche' il flip non e'
-- consolidato il percorso storico (`select_models_for_requirement`) resta il
-- servente a flag OFF e LEGGE entrambe le colonne. Droppare `cost_direction`
-- qui renderebbe il rollback impossibile senza una nuova migrazione. Il drop
-- delle due colonne e' la pulizia dello stadio 3, dopo la finestra di
-- osservazione dello shadow-compare.

BEGIN;

ALTER TABLE nexus_routing_slots_matrix
    ADD COLUMN IF NOT EXISTS base_capability TEXT NULL;

UPDATE nexus_routing_slots_matrix
   SET base_capability = CASE
       WHEN 'code' = ANY(required_capabilities) THEN 'code'
       WHEN COALESCE(array_length(required_capabilities, 1), 0) >= 1
            THEN required_capabilities[1]
       ELSE NULL
   END;

COMMENT ON COLUMN nexus_routing_slots_matrix.base_capability IS
'La capability che DEFINISCE il compito della chiave slot (vocabolario di select_model, F3-02). NULL = nessun filtro capability. Le altre required_capabilities erano preferenza pesata dello scoring del promoter e muoiono con la delega al servizio unico.';

COMMIT;
