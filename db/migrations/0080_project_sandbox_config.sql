-- Configurazione sandbox per-progetto.
-- Sovrascrive le impostazioni globali (settings) per un singolo progetto.
-- NULL significa "usa i default globali".
--
-- Schema del JSONB:
--   memory_mb    INTEGER  Limite memoria container (es. 1024, 2048)
--   cpus         NUMERIC  Limite CPU core (es. 1.0, 2.0, 4.0)
--   network_mode TEXT     Modalità rete Docker: "none" | "bridge" | "host"
--                          - none:   isolamento totale (default per tool agente)
--                          - bridge: accesso internet (utile per npm install)
--                          - host:   condivide stack di rete host (solo servizi)
--   extra_env    JSONB    Variabili d'ambiente aggiuntive iniettate in ogni processo
ALTER TABLE projects
    ADD COLUMN IF NOT EXISTS sandbox_config JSONB;

COMMENT ON COLUMN projects.sandbox_config IS
    'Configurazione sandbox Docker per-progetto. NULL = default globali da settings.';
