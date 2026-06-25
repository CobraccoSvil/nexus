-- 0456_directive_config_restart.sql
-- B3 (gap agente): complemento comportamentale di B2. Direttiva condivisa che
-- ricorda all'agente di riavviare i servizi correlati dopo aver modificato file
-- di configurazione critica, prima di verificarne il comportamento. Senza, il
-- fix (es. il .env del proxy) non veniva applicato e la verifica girava sulla
-- vecchia config (incidente login Beauty-Book: VITE_API_URL corretto nel .env
-- ma frontend non riavviato -> proxy ancora sulla porta vecchia). Stile
-- segnala-non-prescrivi (mig 0438). Scope agent.*, iniettata da prompt_registry.
-- Idempotente.

INSERT INTO nexus_shared_directives (key, content, scope, priority, is_active, description)
VALUES (
    'config_restart',
    E'<config_restart>\nDopo aver modificato un file di CONFIGURAZIONE critica (.env, vite.config.*,\nnext.config.*, package.json, tsconfig*.json, docker-compose.*, Cargo.toml) di un\nprogetto con servizi in esecuzione:\n- I processi gia'' avviati NON applicano le modifiche finche'' non vengono\n  riavviati: Vite/Next leggono .env e i file di config SOLO all''avvio, e il\n  proxy verso /api usa la porta presa all''avvio.\n- Se esiste un servizio attivo del progetto impattato dalla modifica, riavvialo\n  (service_restart) PRIMA di verificarne il comportamento, poi ricontrolla\n  l''esito REALE (es. la chiamata HTTP all''endpoint), non solo che il codice\n  compili.\n</config_restart>',
    'agent',
    40,
    TRUE,
    'B3: ricorda di riavviare i servizi dopo modifiche a config critica prima di verificare l''esito reale. Complemento di B2 (hint in files.rs + criterio HTTP del final_gate). Incidente login Beauty-Book.'
)
ON CONFLICT (key) DO NOTHING;
