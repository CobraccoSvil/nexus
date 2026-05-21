// Mirror dei tipi Rust in `crates/nexus-events/src/event.rs`.
// Tipi manuali (no codegen): tenerli allineati. Schema versionato via SCHEMA_VERSION.

export const SCHEMA_VERSION = 1;

export type EventTopic =
  | "playwright"
  | "ports"
  | "problems"
  | "services"
  | "files"
  | "git"
  | "database"
  | "flags"
  | "monitor"
  | "agent"
  | "notification"
  | "custom"
  | "system"
  | "chat"
  | "mutation"
  | "meta";

export interface UiHint {
  highlight_panel?: string;
  toast_severity?: "info" | "success" | "warning" | "error";
  toast_msg?: string;
  badge_increment?: [string, number];
  flash_duration_ms?: number;
}

// Unione discriminata su `kind` (matcha #[serde(tag = "kind")] in Rust).
export type ProjectEvent =
  | { kind: "JobCreated"; id: string; job_kind: string; status: string; label: string; summary?: string; artifacts?: unknown }
  | { kind: "JobUpdated"; id: string; status: string; label?: string; summary?: string }
  | { kind: "JobsCleared"; job_kind: string; deleted: number }
  | { kind: "PortAllocated"; port: number; label: string; pid?: number }
  | { kind: "PortReleased"; port: number }
  | { kind: "FindingsUpdated"; scan_id?: string; total: number; critical: number; warnings: number; resolved_ids?: string[] }
  | { kind: "ServiceStarted"; name: string; port?: number; pid?: number }
  | { kind: "ServiceStopped"; name: string }
  | { kind: "ServiceRestarted"; name: string }
  | { kind: "FileChanged"; path: string; op: "created" | "modified" | "deleted" }
  | { kind: "GitStatusChanged"; branch: string; ahead: number; behind: number; modified_count: number }
  | { kind: "DbQueryRun"; query_id?: string; duration_ms: number; rows: number; statement_kind: string }
  | { kind: "DbConfigUpdated"; name: string; engine?: string; action: string }
  | { kind: "AgentToolUsed"; run_id: string; tool: string; target_resource?: string }
  | { kind: "Notification"; severity: string; message: string; panel?: string; ttl_ms?: number; run_id?: string }
  | { kind: "FlagChanged"; key: string; value: unknown }
  | { kind: "MonitorUpdated"; monitor_id: string; value: unknown; label?: string }
  | { kind: "HighlightPanel"; panel: string; duration_ms: number }
  | { kind: "Custom"; event_name: string; resource: string; payload: unknown }
  | { kind: "ChatSessionCompacted"; session_id: string; summary_point_id?: string; total_tokens: number; total_cost_usd: number }
  | { kind: "ChatMessageAdded"; session_id: string; message_id: string; role: string; total_tokens?: number; total_cost_usd?: number }
  | { kind: "ChatSessionStatusChanged"; session_id: string; status: string }
  | { kind: "MutationRecorded"; method: string; path: string; status_code: number; session_id?: string; summary?: string; actor_user_id?: string }
  | { kind: "EventEnriched"; event_id: string; ui_hint?: UiHint; semantic_tags?: string[]; severity_inferred?: string; panel_target?: string }
  | { kind: "ProjectCreated"; name: string; slug: string }
  | { kind: "ProjectDeleted"; name: string }
  | { kind: "MigrationApplied"; migration_name: string; version: string }
  | { kind: "MigrationRolledBack"; migration_name: string; version: string }
  | { kind: "RunConfigChanged"; config_id: string; label: string; action: string }
  | { kind: "MemoryUpdated"; category: string; count_delta: number }
  | { kind: "ProviderHealthChanged"; provider: string; status: string; latency_ms?: number }
  | { kind: "PluginChanged"; plugin_id: string; slug: string; action: string }
  | { kind: "SettingChanged"; namespace: string; key: string }
  | { kind: "SubagentRunChanged"; run_id: string; status: string; parent_run_id?: string }
  | { kind: "QualityScanProgress"; scan_id: string; phase: string; percent?: number }
  | { kind: "OutputChannelCreated"; channel_id: string; label: string }
  | { kind: "SnapshotRequired"; reason: string; last_known_seq: number }
  | { kind: "KnowledgeNoteCreated"; note_id: string; title: string; intent: string | null }
  | { kind: "KnowledgeNoteUpdated"; note_id: string; status: string }
  | { kind: "KnowledgeLinkCreated"; link_id: string; from: string; to: string; rel_type: string; created_by: string };

export interface EnvelopedEvent {
  event_id: string;
  seq: number;
  project_id: string;
  ts: number;
  topic: EventTopic;
  payload: ProjectEvent;
  ui_hint?: UiHint;
  schema_version: number;
}

export type ConnectionStatus =
  | "idle"
  | "connecting"
  | "open"
  | "reconnecting"
  | "disconnected";

export interface ToastItem {
  id: string;
  severity: "info" | "success" | "warning" | "error";
  message: string;
  ttl_ms: number;
  panel?: string;
  createdAt: number;
}

export interface MonitorState {
  value: unknown;
  label?: string;
  updated_at?: string;
}

// Riuso del tipo dall'api-client per compatibilita' con BottomPanelManager
export type { PlaywrightRunSummary } from "../api-client";
