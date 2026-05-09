-- Migrazione 0124: flag agent_router_enabled nella tabella settings
--
-- Il server gRPC AgentRouter (porta 50072) espone il Q-Learning router di
-- nexus-orchestrator al brain Python. Era attivato tramite env var
-- ENABLE_AGENT_ROUTER=1 nel .env locale, che richiedeva un rideploy per
-- ogni cambio e non era visibile dall'admin panel.
--
-- Con questa migrazione diventa un'impostazione amministrativa standard,
-- controllabile da /admin/settings (categoria 'agent') senza rideploy.
-- mcp-core legge il valore all'avvio tramite get_setting(); l'env var
-- ENABLE_AGENT_ROUTER resta come override di emergenza (priorita' piu' alta).
--
-- Categoria 'agent': raggruppa parametri comportamentali degli agenti AI.

INSERT INTO settings (key, value, category, description, is_secret) VALUES

    ('agent_router_enabled',
     'true',
     'agent',
     'Abilita il server gRPC AgentRouter (porta 50072) che espone il router '
     'Q-Learning di nexus-orchestrator al brain Python. Quando attivo, il '
     'router_node consulta il Q-Learning per scegliere il profilo agente '
     'ottimale (es. coder, cloud_architect, tech_writer) in base alla cronologia '
     'dei reward osservati. Se disabilitato il brain usa il routing di fallback '
     'basato solo sull''intent. Richiede riavvio di mcp-core per applicare la modifica.',
     FALSE)

ON CONFLICT (key) DO UPDATE
    SET description = EXCLUDED.description,
        category    = EXCLUDED.category;
