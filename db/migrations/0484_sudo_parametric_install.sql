-- 0484_sudo_parametric_install.sql
-- Permette alla chat Nexus di installare dipendenze di SISTEMA (apt) in modo
-- controllato, estendendo il Sudo Manager Livello 1 (ADR 0017) con purpose
-- PARAMETRICI: il command_template resta fisso (programma + flag) ma il chiamante
-- (sudo_manager) puo' appendere argomenti EXTRA (nomi pacchetto) validati contro
-- un pattern stretto sia lato runner (defense-in-depth hardcoded) sia lato DB.
--
-- Causa radice (incident reale, regola H): per i test E2E Playwright l'agente
-- deve installare librerie di sistema (apt-get install libnss3 ...). Finora
-- poteva farlo SOLO col purpose 'playwright-install-deps' (lista FISSA) e
-- comunque non sapeva invocarlo: tentava `sudo apt install ...` via run_command,
-- che fallisce perche' il NOPASSWD e' concesso SOLO al binary nexus-sudo-runner,
-- mai a `sudo` arbitrario. Risultato: run bloccato 16 min su "Failed to install
-- browsers".
--
-- Fix definitivo (regola H, niente toppa; regola L, punto unico = sudo_manager):
--   (a) colonna allows_extra_args: solo i purpose marcati true accettano args
--       extra dal chiamante. Tutti i purpose esistenti restano a false ->
--       backward-compatible (rifiutano args extra esattamente come prima).
--   (b) purpose 'apt-install' parametrico: `apt-get install -y <pacchetti>`,
--       dove i nomi pacchetto arrivano come args extra dal chiamante.
--   (c) blocco <privilegi_sistema> nei system prompt agente: istruisce il
--       modello a usare `sudo apt-get install -y <pkg>` via run_command (che
--       Nexus instrada automaticamente al sudo-runner), NON sudo arbitrario.
--
-- Sicurezza INVARIATA: programmi in PATH_ALLOWLIST hardcoded nel runner; nessuna
-- shell; env-clean; audit log immutabile. Gli args extra passano un pattern
-- nome-pacchetto stretto (^[a-z0-9][a-z0-9._+-]*$): niente flag (no `-` iniziale),
-- niente path, niente metacaratteri. Il sudo arbitrario resta impossibile.

BEGIN;

-- ── (a) Colonna allows_extra_args ──────────────────────────────────────────
ALTER TABLE nexus_sudo_purposes
    ADD COLUMN IF NOT EXISTS allows_extra_args BOOLEAN NOT NULL DEFAULT false;

COMMENT ON COLUMN nexus_sudo_purposes.allows_extra_args IS
    'Se true, sudo_manager puo'' appendere argomenti EXTRA al command_template; '
    'il runner li valida contro un pattern nome-pacchetto stretto '
    '(^[a-z0-9][a-z0-9._+-]$). Default false: i purpose a lista fissa rifiutano '
    'args extra (backward-compatible).';

-- ── (b) Purpose parametrico apt-install ────────────────────────────────────
-- command_template = programma + flag fissi (apt-get install -y); i nomi
-- pacchetto sono passati come args extra dal chiamante e validati lato runner.
-- ON CONFLICT DO UPDATE: idempotente e ri-applicabile (regola H).
INSERT INTO nexus_sudo_purposes
    (name, description, command_template, category, requires_confirm, allows_extra_args)
VALUES
    (
        'apt-install',
        'Installa pacchetti di sistema via APT (apt-get install -y <pacchetti>). I nomi pacchetto sono passati come argomenti extra dal chiamante e validati contro un pattern nome-pacchetto stretto (niente flag/path/metacaratteri). Usato dalla chat per installare dipendenze di sistema (es. librerie Playwright/Chromium, toolchain).',
        'apt-get install -y',
        'general',
        false,
        true
    )
ON CONFLICT (name) DO UPDATE
    SET command_template = EXCLUDED.command_template,
        description      = EXCLUDED.description,
        category         = EXCLUDED.category,
        requires_confirm = EXCLUDED.requires_confirm,
        allows_extra_args = EXCLUDED.allows_extra_args,
        enabled          = TRUE,
        updated_at       = NOW();

-- ── (c) Blocco <privilegi_sistema> nei system prompt agente ────────────────
-- Append idempotente: la seconda esecuzione e' no-op (NOT LIKE diventa falso).
-- Spiega al modello come installare dipendenze di sistema passando dal
-- sudo-runner controllato, e i vincoli (no sudo arbitrario).
UPDATE nexus_prompt_templates
SET content = content || E'\n\n<privilegi_sistema>\n'
    || E'Puoi installare dipendenze di SISTEMA che richiedono privilegi di root '
    || E'(es. librerie per Playwright/Chromium, toolchain, pacchetti apt) eseguendo '
    || E'il comando con run_command:\n'
    || E'  sudo apt-get install -y <pacchetto> [<pacchetto> ...]\n'
    || E'Nexus instrada automaticamente questo comando al gestore privilegiato '
    || E'controllato (sudo-runner, ADR 0017): non serve password e l''esecuzione e'' '
    || E'tracciata nell''audit log. Funzionano anche `apt-get install`/`apt install` '
    || E'senza `sudo`, e `apt-get update`.\n'
    || E'Per i browser Playwright: `npx playwright install <browser>` NON richiede '
    || E'sudo (va nella cache utente); solo le librerie di sistema (`--with-deps`) '
    || E'passano dal sudo-runner.\n'
    || E'VINCOLI: e'' permesso SOLO apt-get/apt (installazione pacchetti) e la '
    || E'gestione dei servizi del progetto. Il sudo ARBITRARIO non e'' consentito '
    || E'(niente `sudo rm`, `sudo chmod`/`chown` fuori progetto, ecc.): se un comando '
    || E'privilegiato non e'' instradabile riceverai un messaggio che lo spiega. Usa '
    || E'nomi pacchetto validi (niente flag extra come --allow-*). Non reinstallare '
    || E'pacchetti gia'' presenti: apt salta automaticamente quelli installati.\n'
    || E'</privilegi_sistema>'
WHERE key IN ('system.nexus_base', 'agent.coder.base', 'agent.general.debugger')
  AND content NOT LIKE '%<privilegi_sistema>%';

COMMIT;
