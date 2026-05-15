-- Fix M58: estendi CHECK constraint allocation_mode in nexus_port_allocations.
--
-- Sintomo iter_8 (run badd7d4f, step 22): request_port falliva con
--   "new row violates check constraint nexus_port_allocations_allocation_mode_check"
-- perche' il codice Rust in project_workspace/allocate_port.rs:64 inserisce
-- allocation_mode='dynamic' ma il CHECK originale ammette solo
-- ARRAY['auto','manual']. Anche allocate_port.rs ritorna 'existing' come
-- valore semantico per le porte gia' note.
--
-- Decisione: estendere il vocabolario lato DB invece di restringere il codice,
-- perche' 'dynamic' (bucket deterministico) e 'existing' (riuso) hanno
-- semantiche distinte da 'auto'/'manual' e vanno preservate per audit.

ALTER TABLE nexus_port_allocations
    DROP CONSTRAINT IF EXISTS nexus_port_allocations_allocation_mode_check;

ALTER TABLE nexus_port_allocations
    ADD CONSTRAINT nexus_port_allocations_allocation_mode_check
        CHECK (allocation_mode IN ('auto', 'manual', 'dynamic', 'existing'));
