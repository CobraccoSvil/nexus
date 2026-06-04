---
id: 78380cec-71b4-4718-b9c1-9a9f71c0600b
kind: other
title: Pattern MCP tool (agent_tools)
slug: pattern-mcp-tool
tags:
  - concept
  - pattern
  - mcp
  - agent
source_files:
  - crates/mcp-core/src/meta_docs/generators/concepts.rs
auto_generated: true
created_at: 2026-05-23T11:09:00Z
updated_at: 2026-06-04T10:14:05Z
nexus_meta_version: 1
---

# Pattern MCP tool

Gli **MCP tools** sono funzioni callable dall'agent loop (e da Claude Code via MCP server).

## Tool esistenti (350+ in `crates/mcp-core/src/agent_tools/`)

- **File**: `read_file`, `write_file`, `edit_file`, `delete_file`, `list_files`, `search_in_files`
- **Git**: `git_commit`, `git_push`, `git_pull`, `git_status`, `git_stage`
- **Service**: `run_service`, `list_active_services`, `read_service_output`
- **Testing**: `run_playwright_tests`, `run_lint_fix`
- **Nexus orchestration**: `nexus_subagent_*`, `nexus_todo_write`, `nexus_mcp_tool_*`
- **Sandbox**: `get_sandbox_config`
- **Dispatcher**: `dispatcher_emit_event`, `dispatcher_post_notification`

## Aggiungere un tool

1. `crates/mcp-core/src/agent_tools/my_tool.rs`
2. `pub async fn tool_my_tool(ctx: &AgentToolContext, input: &Value) -> String`
3. Esposto in `agent_tools/mod.rs`
4. Schema JSON in `AGENT_TOOLS_JSON`
5. Dispatcher case in `agent_loop.rs`

Vedi [[mcp-tools]] per la lista completa.
