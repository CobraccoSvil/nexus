-- 0433_service_restart_sudo_purposes.sql
-- Completa la governance di restart dei servizi --SYSTEM via Sudo Manager
-- (ADR 0017 + 0028), estendendo il pattern di 0416 (brain-restart) agli altri
-- tre servizi Nexus gestiti da systemd --system.
--
-- Problema (incidente reale osservato 2026-06-15): nexus-gateway.service in stato
-- `failed` da ore perche' un processo gateway legacy (avviato fuori systemd nella
-- sessione precedente) occupava la porta 4060 -> l'unit non faceva il bind ->
-- exit 1 -> StartLimit esaurito. Per ripristinarlo serviva `systemctl restart
-- nexus-gateway.service`, ma a differenza del brain NON esisteva un purpose sudo
-- corrispondente: solo `brain-restart` (mig 0416) era definito. Senza purpose, ne'
-- il watchdog ne' un operatore non interattivo possono riavviare gateway/mcp-core/
-- web-ide senza password -> i processi orfani diventano la norma (toppa, regola H).
--
-- Fix definitivo (regola L: unico canale privilegiato = nexus-sudo-runner, mig
-- 0289; regola H: causa = governance incompleta, non il singolo restart manuale):
-- si aggiungono i tre purpose mancanti, identici per forma a brain-restart, sotto
-- la stessa validazione PATH_ALLOWLIST (systemctl gia' incluso) + ARG_SAFE_PATTERN.
-- Ogni command_template e' delimitato a UNA sola unit. requires_confirm=false:
-- azione automatica d'infrastruttura, come brain-restart.
--
-- Con questi purpose il watchdog/auto-remediation puo' ripristinare qualunque
-- servizio --system via sudo_manager::execute(db, "<svc>-restart"), e la
-- transizione dei processi legacy sotto systemd non richiede piu' la password.
--
-- Regola G: config nel DB, nessun nuovo binario root. Idempotente
-- (ON CONFLICT (name) DO NOTHING): ri-applicabile dopo wipe + re-migrazione.

BEGIN;

INSERT INTO nexus_sudo_purposes (name, description, command_template, category, requires_confirm)
VALUES (
    'gateway-restart',
    'Riavvia il Nexus Gateway LLM (nexus-gateway.service, unit systemd --system, porta 4060, ADR 0028 L3). Usato per ripristinare il transport LLM quando l''unit e'' caduta (es. porta occupata da un processo legacy, provider chain da ricaricare) senza intervento manuale con password.',
    'systemctl restart nexus-gateway.service',
    'service',
    FALSE
)
ON CONFLICT (name) DO NOTHING;

INSERT INTO nexus_sudo_purposes (name, description, command_template, category, requires_confirm)
VALUES (
    'mcp-core-restart',
    'Riavvia il Nexus MCP Core (nexus-mcp-core.service, unit systemd --system, ADR 0028 L3). Usato per applicare nuove migrazioni DB (caricate all''avvio via sqlx migrate!) o ripristinare l''orchestratore quando un task tokio resta stuck nonostante il processo sia vivo.',
    'systemctl restart nexus-mcp-core.service',
    'service',
    FALSE
)
ON CONFLICT (name) DO NOTHING;

INSERT INTO nexus_sudo_purposes (name, description, command_template, category, requires_confirm)
VALUES (
    'web-ide-restart',
    'Riavvia la Nexus Web IDE (nexus-web-ide.service, unit systemd --system, ADR 0028 L3). Usato per ripristinare il frontend Next.js (build di produzione, niente HMR) dopo un deploy o un crash.',
    'systemctl restart nexus-web-ide.service',
    'service',
    FALSE
)
ON CONFLICT (name) DO NOTHING;

COMMIT;
