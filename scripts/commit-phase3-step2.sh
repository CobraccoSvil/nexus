#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add crates/mcp-core/src/mcp_client.rs \
        crates/plugin-service/src/mcp_client.rs \
        crates/nexus-http/src/lib.rs \
        crates/mcp-core/src/agent_processes.rs \
        crates/mcp-core/src/auth.rs \
        crates/mcp-core/src/nexus_builtin/prompt_admin.rs \
        crates/mcp-core/src/nexus_tools/git_blame.rs \
        crates/mcp-core/src/nexus_tools/openapi_validate.rs \
        crates/mcp-core/src/nexus_tools/profile_run.rs \
        crates/mcp-core/src/orchestrator.rs \
        crates/mcp-core/src/profiles.rs \
        crates/mcp-core/src/prompt_templates.rs \
        crates/mcp-db/src/lib.rs \
        crates/ruvector/src/core.rs \
        crates/mcp-core/src/main.rs \
        crates/mcp-core/src/project_workspace/sync_ports.rs \
        scripts/commit-phase3-step2.sh

git commit -m "$(cat <<'EOF'
chore(rust): elimina unwrap reali fuori test (Fase 3 step 2)

CLAUDE.md §F: niente `unwrap()`/`expect()` fuori `#[cfg(test)]` salvo
eccezioni documentate. Affrontate 15 occorrenze categorizzate come "veri"
fix nel report `classify-unwrap.py`:

Process I/O (3+3 → ok_or_else con McpError::Protocol):
  - crates/mcp-core/src/mcp_client.rs: child.stdin/stdout/stderr.take()
  - crates/plugin-service/src/mcp_client.rs: stesso pattern

Option handling reale:
  - crates/mcp-core/src/agent_processes.rs:93 — project_root con Result<_,String>
  - crates/mcp-core/src/main.rs:211 — pid in re-attach branch (let-else + continue)
  - crates/mcp-core/src/nexus_builtin/prompt_admin.rs:36 — if-let su category_filter
  - crates/mcp-core/src/nexus_tools/openapi_validate.rs:75 — match su spec.get("info")
  - crates/mcp-core/src/orchestrator.rs:2386 — unwrap_or_default su suggested_model
  - crates/mcp-core/src/profiles.rs:404 — map + unwrap_or_else su nth(idx)
  - crates/mcp-core/src/prompt_templates.rs:868 — Value::Array pattern invece di guard+unwrap
  - crates/mcp-db/src/lib.rs:34 — fallback ParserError se dialects vuoto
  - crates/ruvector/src/core.rs:278 — let-else
  - crates/mcp-core/src/nexus_tools/git_blame.rs:102 — let-else dopo peek
  - crates/mcp-core/src/nexus_tools/profile_run.rs:32 — index esplicito post is_empty
  - crates/mcp-core/src/auth.rs:307 — unwrap_or_else con Response default

Configurazione e Regex:
  - crates/nexus-http/src/lib.rs:66,71 — if-let invece di is_some()+unwrap()
  - crates/nexus-http/src/lib.rs:110 — annotazione safety su Client::build()
  - crates/mcp-core/src/project_workspace/sync_ports.rs — header // safety: per
    cluster Regex literal §F (2 occorrenze sopravvissute al classifier)

Build: `cargo check --workspace` resta verde.
EOF
)"

git log -1 --stat
