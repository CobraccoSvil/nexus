INSERT INTO settings (key, value, category, description, is_secret) VALUES
    ('provider_hierarchy', 'anthropic,openai,google', 'routing', 'Ordered fallback chain for chat providers', FALSE),
    ('provider_model_anthropic', 'claude-sonnet-4-6', 'routing', 'Preferred Anthropic model for chat routing', FALSE),
    ('provider_model_openai', 'gpt-4o-mini', 'routing', 'Preferred OpenAI model for chat routing', FALSE),
    ('provider_model_google', 'gemini-2.5-flash', 'routing', 'Preferred Google model for chat routing', FALSE),
    ('routing_fix_providers', 'anthropic,openai,google', 'routing', 'Provider order for fix requests', FALSE),
    ('routing_refactor_providers', 'anthropic,openai,google', 'routing', 'Provider order for refactor requests', FALSE),
    ('routing_test_providers', 'openai,anthropic,google', 'routing', 'Provider order for test requests', FALSE),
    ('routing_docs_providers', 'openai,anthropic,google', 'routing', 'Provider order for documentation requests', FALSE),
    ('routing_architecture_providers', 'anthropic,openai,google', 'routing', 'Provider order for architecture requests', FALSE),
    ('routing_chat_providers', 'openai,anthropic,google', 'routing', 'Provider order for general chat requests', FALSE)
ON CONFLICT (key) DO NOTHING;
