-- Migrazione 0194: configurazione vision_describe per nexus_describe_image_attachment.
--
-- Aggiunge il purpose 'vision_describe' usato dal tool agente per chiamare
-- l'endpoint POST /vision/describe del brain, e il setting che fissa il
-- limite massimo di dimensione immagine processabile.
--
-- Niente fallback hardcoded lato codice (regola G di CLAUDE.md): se questa
-- migrazione non e' applicata il tool ritorna errore esplicito al modello.
--
-- Idempotente via ON CONFLICT DO NOTHING.
--
-- Riferimenti:
--   - Tool: crates/mcp-core/src/agent_tools/vision_tools.rs
--   - Endpoint brain: POST /vision/describe in brain/grpc_server/main.py
--   - ADR 0011 (estensione vision): docs/.nexus-vault/adr/0011-attachment-inspection-pipeline.md

-- Modello dedicato all'analisi visiva di immagini allegate. Default Google
-- Gemini 2.0 Flash (supporto multimodale nativo, latenza bassa, costo basso).
-- Modificare via UI admin -> nexus_purpose_model -> vision_describe per
-- cambiare provider/modello senza redeploy.
INSERT INTO nexus_purpose_model (purpose, provider, model_id, notes, updated_at)
VALUES (
    'vision_describe',
    'google',
    'gemini-2.0-flash-exp',
    'Modello vision usato da nexus_describe_image_attachment per descrivere immagini allegate (mockup, screenshot, foto, diagrammi).',
    now()
)
ON CONFLICT (purpose) DO NOTHING;

-- Limite dimensione immagine processabile dal tool vision. Configurabile via
-- settings -> agent.attachment.image_max_bytes. Oltre il limite il tool
-- ritorna errore esplicito al modello (no truncation silenziosa).
INSERT INTO settings (key, value, category, description, is_secret)
VALUES (
    'agent.attachment.image_max_bytes',
    '2097152',
    'agent',
    'Massima dimensione (byte) di un immagine processabile dal tool nexus_describe_image_attachment. Default 2 MB. Oltre il limite il tool ritorna errore esplicito al modello.',
    FALSE
)
ON CONFLICT (key) DO NOTHING;
