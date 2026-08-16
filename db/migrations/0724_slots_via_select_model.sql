-- 0724_slots_via_select_model.sql (F3-04, fase 3 lotto 2)
--
-- Flag di rollout della delega del canale SLOT al servizio unico di selezione
-- (`select_model`), piu' l'interruttore dello shadow-compare dello stadio 1.
--
-- Il difetto che accompagna: il canale slot — il PRIMO routing tentato quando
-- il classificatore estrae slot affidabili — era l'ultimo percorso di
-- selezione del turno primario fuori dal servizio unico. Sceglieva con lo
-- scoring a pesi del promoter (select_models_for_requirement) e applicava il
-- cooldown DOPO la dedup top-1-per-provider, per solo FORNITORE: niente
-- pavimento di tier agentico (I8), coppia satura servita anche col modello
-- sano dello stesso fornitore disponibile, esito non tipizzato, niente
-- riordino cache-aware (mig 0721) ne' governato (ADR 0030).
--
-- Rollout in stadi, NON cutover secco: la delega cambia piu' assi insieme
-- (pavimento, cooldown per coppia, riordino) e senza una finestra di
-- osservazione non si saprebbe quale asse spiega una divergenza percepita in
-- chat. Precedente in repo: routing.per_intent_runtime_shadow (FASE 3 ADR
-- 0030).
--
--   Stadio 1 (questa migrazione): slots_via_select_model='false' -> serve il
--     percorso storico, bit-identico; lo shadow-compare calcola la delega in
--     parallelo e logga la divergenza (target routing_shadow).
--   Stadio 2: flip a 'true' via UPDATE settings dopo la finestra di
--     osservazione. Rollback = flip a 'false', zero deploy.
--   Stadio 3 (fuori da questa migrazione): rimozione del percorso storico e
--     delle colonne che solo lui legge (required_capabilities,
--     cost_direction della slot-matrix).
--
-- Consumatore (regola G, nessun default acceso nel codice):
--   mcp-core::orchestrator::slot_routing
--
-- Idempotente: INSERT ... ON CONFLICT DO NOTHING.

INSERT INTO settings (key, value, category, description) VALUES
(
    'routing.slots_via_select_model', 'false', 'routing',
    'Se ''true'', il canale di routing slot-based (nexus_routing_slots_matrix) delega la scelta provider/modello al servizio unico select_model: pavimento di tier agentico (agent.routing.agentic_min_tier), cooldown per coppia PRIMA della scelta, gate di qualificazione, esito tipizzato e riordino CostFirst cache-aware (mig 0721). A ''false'' (default) serve il percorso storico dello scoring a pesi, bit-identico, con shadow-compare della delega (vedi routing.slots_select_model_shadow). Rollback = flip a false, zero deploy.'
),
(
    'routing.slots_select_model_shadow', 'true', 'routing',
    'Se ''true'' e routing.slots_via_select_model=''false'', ogni decisione slot-based calcola IN PARALLELO la delega a select_model e logga la divergenza (provider, modello) storico vs delega sul target routing_shadow, senza cambiare la decisione servita. E'' la finestra di osservazione dello stadio 1: le divergenze attese sono pavimento di tier, cooldown per coppia e riordino di costo. Spegnibile senza deploy se rumoroso; inerte quando la delega e'' attiva.'
)
ON CONFLICT (key) DO NOTHING;
