-- Migrazione 0222: settings per l'export DB -> .claude/agents (Componente A).
--
-- Il generatore Rust claude_agents proietta le definizioni autoritative del DB
-- nei file .claude/agents/<name>.md per Claude Code CLI. Una sola fonte di
-- verita' (DB), file read-only marcati AUTO-GENERATO. overwrite_unmanaged_default
-- = false protegge i 7 file curati a mano finche' non si lancia la promozione
-- esplicita (force_overwrite_unmanaged=true sull'endpoint /regenerate).

INSERT INTO settings (key, value, category, description) VALUES
    ('claude_agents.export_enabled', 'true', 'claude_agents',
     'Abilita la generazione dei file .claude/agents/*.md dalle definizioni DB.'),
    ('claude_agents.output_dir', '.claude/agents', 'claude_agents',
     'Directory di output (relativa a NEXUS_REPO_ROOT) per i file agente generati.'),
    ('claude_agents.name_prefix', 'nexus-', 'claude_agents',
     'Prefisso del nome file/agente generato (kind rust_implementer -> nexus-rust-implementer.md).'),
    ('claude_agents.overwrite_unmanaged_default', 'false', 'claude_agents',
     'Se false, i file senza marker AUTO-GENERATO (curati a mano) NON vengono sovrascritti dalla rigenerazione di default.'),
    ('claude_agents.regen_on_post_commit', 'false', 'claude_agents',
     'Se true, un hook post-commit rigenera i file (opzionale; default off per non rallentare i commit).')
ON CONFLICT (key) DO NOTHING;
