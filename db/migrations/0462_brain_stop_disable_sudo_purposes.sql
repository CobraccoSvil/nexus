-- 0462_brain_stop_disable_sudo_purposes.sql
-- Zero-Python (eliminazione brain): aggiunge i purpose sudo per FERMARE e
-- DISABILITARE il servizio systemd --system nexus-brain.service.
--
-- Contesto: dopo il porting completo a Rust (grafo+classifier+embed+completion+
-- agent_turn+vision+sub-agenti+batch+docx), mcp-core non chiama piu' il brain
-- Python a runtime (engine='rust', classifier_engine='rust'). Il brain va spento
-- e poi eliminato. Esisteva solo 'brain-restart' (mig 0416); servono stop+disable.
--
-- nexus-sudo-runner valida il programma contro la PATH_ALLOWLIST hardcoded (che
-- include gia' systemctl): nessun rebuild del runner necessario, basta il purpose
-- in DB. Idempotente.

INSERT INTO nexus_sudo_purposes (name, description, command_template, enabled) VALUES
    ('brain-stop',    'Ferma il servizio systemd nexus-brain (zero-Python: eliminazione brain)',    'systemctl stop nexus-brain.service',    true),
    ('brain-disable', 'Disabilita il servizio systemd nexus-brain (zero-Python: eliminazione brain)', 'systemctl disable nexus-brain.service', true)
ON CONFLICT (name) DO NOTHING;
