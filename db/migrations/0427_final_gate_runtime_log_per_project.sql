-- 0427_final_gate_runtime_log_per_project.sql
-- Hardening qualita' agentico (fix 2026-06-15): il criterio service_logs_clean
-- del final_gate eseguiva un unico `runtime_log_command` HARDCODED come setting
-- globale (docker compose logs ...). Era cieco per i progetti gestiti via
-- systemd --user / --system (es. Beauty-Book con {slug}-backend.service,
-- {slug}-frontend.service): docker compose ritornava vuoto/errore e il gate
-- chiudeva senza vedere errori runtime evidenti nei log dei servizi reali.
--
-- Risoluzione PER-PROGETTO (regola L, un solo punto di verita': la funzione
-- `_resolve_log_command` in brain/agents/final_gate.py; questa migrazione
-- aggiunge solo le fonti dati che la funzione consulta in ordine).
--
-- Settings introdotti:
--   agent.final_gate.runtime_log_command_per_project
--     Override admin esplicito: JSON object {project_id: "comando"}. Quando
--     presente, vince su tutto (l'admin sa esattamente cosa vuole eseguire).
--     Default: '{}' (nessun override).
--
--   agent.final_gate.runtime_log_command_systemd
--     Comando di default per progetti gestiti via systemd --user. Usa il
--     placeholder {slug} (sostituito dal name del progetto a runtime, stesso
--     algoritmo di crates/mcp-core/src/project_workspace/logs.rs: lowercase +
--     spaces/underscore -> dash). Aggrega:
--      - journalctl --user --user-unit '{slug}-*' (se systemd --user attivo)
--      - tail dei /tmp/nexus-proj-{slug}-*.log (fallback detached, WSL)
--     Il `2>/dev/null` evita che journalctl/tail facciano fallire il pipe in
--     ambienti dove non esistono.
--
-- L'admin puo' aggiungere override puntuali senza redeploy; cache 60s lato
-- brain (orchestrator_config). Idempotente.
INSERT INTO settings (key, value, category, description) VALUES
  ('agent.final_gate.runtime_log_command_per_project', '{}', 'agent',
   'Override per-progetto del comando log usato dal criterio service_logs_clean del final_gate. JSON object {project_id_uuid: "comando shell"}. Vince su qualunque altro setting. Default vuoto: l''auto-detect per-stack decide.'),
  ('agent.final_gate.runtime_log_command_systemd', 'journalctl --user --user-unit "{slug}-*" --no-pager -n 200 2>/dev/null; for f in /tmp/nexus-proj-{slug}-*.log; do [ -f "$f" ] && echo "===== $f =====" && tail -n 200 "$f"; done 2>/dev/null', 'agent',
   'Comando log per progetti gestiti via systemd --user (Beauty-Book et al.). Il placeholder {slug} viene sostituito a runtime dal name del progetto normalizzato (lowercase, spazi/underscore -> dash). Aggrega journalctl --user e i logfile detached /tmp/nexus-proj-{slug}-*.log (fallback WSL).')
ON CONFLICT (key) DO NOTHING;
