-- 0404_system_offload_sections.sql
--
-- P2 roadmap contesto: fix della DECAPITAZIONE del system prompt.
--
-- Prima: _offload_system_prompt_if_huge tagliava il system alla testa di
-- 3.200 char quando superava agent.context.system_prompt_offload_threshold_tokens
-- (8000). Il solo system.nexus_base pesa 8.043 token: il taglio scattava SEMPRE
-- e il modello non vedeva MAI le direttive operative (AGENT_ACT_FIRST_SUFFIX,
-- anti-loop, safety, task playbook appeso in coda, KB) — concausa dei sintomi
-- "descrive invece di eseguire", loop esplorativi, reroute G1.
--
-- Ora (codice, stesso commit): offload PER SEZIONI — solo i blocchi INFORMATIVI
-- a tag chiuso vengono spostati in Qdrant con pointer; le direttive restano
-- SEMPRE inline. Taglio head solo come emergenza >2x soglia (loggato ERROR).
-- Questo setting elenca i tag offloadabili (CSV), modificabile da admin.

INSERT INTO settings (key, value, category, description)
VALUES (
    'agent.context.system_offload_sections',
    'examples,reflection,knowledge_base_progetto',
    'agent',
    'Sezioni (tag XML) del system prompt offloadabili in RAG quando il system supera la soglia di offload: blocchi informativi recuperabili on-demand. Le direttive operative (role/autonomia/protocollo/anti_loop/safety/playbook) restano SEMPRE inline.'
)
ON CONFLICT (key) DO NOTHING;
