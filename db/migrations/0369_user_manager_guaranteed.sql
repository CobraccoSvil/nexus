-- 0369_user_manager_guaranteed.sql
-- Gestione processi permanente e affidabile di Nexus su WSL (ADR 0028).
--
-- Problema: in WSL il systemd --user MANAGER (user@<UID>.service) non viene
-- triggerato deterministicamente al boot anche con linger abilitato (manca il
-- trigger di login PAM). Quando muore, `systemctl --user` da "Connection
-- refused": il service_observer diventa cieco e i servizi dei progetti non sono
-- gestibili. Fix definitivo a 2 livelli:
--   L1 (garanzia reale): unit systemd --SYSTEM oneshot al boot
--      (/etc/systemd/system/nexus-user-manager.service, installata da
--      deploy/install-user-manager.sh) che fa enable-linger + start user@<UID>.
--   L2 (cintura race-window): mcp-core all'avvio chiama ensure_user_manager(),
--      che se il bus e' giu lo risuscita via il nexus-sudo-runner gia' esistente
--      (Sudo Manager, mig 0289) usando QUESTO purpose.
--
-- Regola G: config nel DB. Regola L: riusa il Sudo Manager (mig 0289) come unico
-- canale privilegiato; niente nuovo binario root. Idempotente.

BEGIN;

-- Purpose sudo per la risurrezione del manager utente. Il runner
-- (crates/nexus-sudo-runner) esegue command_template dopo validazione contro
-- PATH_ALLOWLIST (systemctl gia' incluso) e ARG_SAFE_PATTERN (user@<UID>.service
-- e' valido: @ . - ammessi). NB: il runner NON sostituisce placeholder e il
-- carattere '%' NON e' ammesso negli argomenti -> il command_template deve
-- contenere l'UID NUMERICO reale. 1000 e' il default WSL del primo utente:
-- deploy/install-user-manager.sh fa UPDATE di questo template con l'UID reale
-- (id -u) a install-time, quindi nessun magic number e' load-bearing nel codice.
-- `systemctl start user@<UID>.service` (senza --user) gira come root via il
-- runner e avvia il manager utente: e' un comando di sistema benigno e delimitato
-- a una sola unit. requires_confirm=false: e' un'azione automatica d'infrastruttura.
INSERT INTO nexus_sudo_purposes (name, description, command_template, category, requires_confirm)
VALUES (
    'user-manager-start',
    'Avvia il systemd --user manager (user@<UID>.service) quando il bus utente e'' giu'' in WSL. Usato da mcp-core ensure_user_manager() per ripristinare la gestione dei servizi di progetto. L''UID reale e'' iniettato a install-time da deploy/install-user-manager.sh.',
    'systemctl start user@1000.service',
    'service',
    FALSE
)
ON CONFLICT (name) DO NOTHING;

-- Settings (regola G): governano il Livello 2 (on-startup di mcp-core).
-- La unit --system del Livello 1 resta attiva indipendentemente da questi flag.
INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.user_manager.autostart_enabled', 'true', 'agent',
     'Se true, mcp-core all''avvio (ensure_user_manager) risuscita il systemd --user manager via il purpose sudo user-manager-start quando il bus e'' giu''. La garanzia di boot (unit --system nexus-user-manager.service) resta attiva anche se questo flag e'' false.',
     NOW()),
    ('agent.user_manager.resurrection_cooldown_seconds', '120', 'agent',
     'Intervallo minimo tra due tentativi di risurrezione del manager utente da parte di mcp-core. Nel codice esiste un FLOOR non bypassabile di 60s anche se questo valore e'' inferiore, per evitare di martellare root se user@<UID> e'' in crash-loop.',
     NOW())
ON CONFLICT (key) DO NOTHING;

COMMIT;
