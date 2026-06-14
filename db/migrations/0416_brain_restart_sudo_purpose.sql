-- 0416_brain_restart_sudo_purpose.sql
-- Auto-recovery del brain --SYSTEM via Sudo Manager (ADR 0017 + 0028).
--
-- Problema: task_watchdog::try_restart_systemd_or_process usava
-- `systemctl --user restart nexus-brain.service`, ma dopo ADR 0028 L3 il brain e'
-- una unit systemd --SYSTEM (gestita da PID 1). Il ramo --user non toccava piu' il
-- servizio giusto e il fallback (pgrep + log) non riavviava nulla: l'auto-recovery
-- del watchdog era di fatto rotto. Quando l'embedder (che gira DENTRO il brain) va
-- in timeout pur essendo il processo vivo, systemd non vede un'uscita e quindi NON
-- restarta (Restart=always copre solo l'uscita del processo, non il "vivo ma
-- bloccato"): l'unico che puo' forzare il restart e' il watchdog.
--
-- Fix definitivo (regola L: unico canale privilegiato = nexus-sudo-runner, mig
-- 0289): il watchdog ora chiama sudo_manager::execute(db, "brain-restart") che
-- esegue il command_template sotto validazione PATH_ALLOWLIST (systemctl gia'
-- incluso) + ARG_SAFE_PATTERN. `systemctl restart nexus-brain.service` gira come
-- root via il runner (per root --system e' il default). Comando di sistema benigno
-- delimitato a una sola unit. requires_confirm=false: azione automatica d'infra.
--
-- Regola G: config nel DB, nessun nuovo binario root. Idempotente.

BEGIN;

INSERT INTO nexus_sudo_purposes (name, description, command_template, category, requires_confirm)
VALUES (
    'brain-restart',
    'Riavvia il Nexus Neural Core (nexus-brain.service, unit systemd --system, ADR 0028 L3). Usato da task_watchdog quando l''embedder gRPC va in timeout pur essendo il processo brain vivo (canale bloccato): systemd non vede un''uscita e non restarta da solo, il watchdog forza il riavvio via questo purpose.',
    'systemctl restart nexus-brain.service',
    'service',
    FALSE
)
ON CONFLICT (name) DO NOTHING;

COMMIT;
