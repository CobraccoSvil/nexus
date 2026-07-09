// Bridge centralizzato: eventi SSE dispatcher -> CustomEvent DOM / invalidazioni store.
// Punto unico (regola L): i pannelli ascoltano questi eventi invece di dipendere
// da bridge sparsi o listener morti.

import type { EnvelopedEvent } from "./types";
import { useProjectStore } from "./store";

/** Eventi SSE il cui `kind_name()` deve essere registrato su EventSource. */
export const DISPATCHER_EVENT_KINDS = [
  "JobCreated",
  "JobUpdated",
  "JobsCleared",
  "PortAllocated",
  "PortReleased",
  "FindingsUpdated",
  "QualityScanProgress",
  "ServiceStarted",
  "ServiceStopped",
  "ServiceRestarted",
  "ServiceStatusChanged",
  "ServiceCrashDetected",
  "ServiceBuildErrors",
  "ServiceDiagnosisStarted",
  "ServiceAnomaly",
  "ServiceLogLine",
  "ServiceMetrics",
  "FileChanged",
  "GitStatusChanged",
  "DbQueryRun",
  "DbConfigUpdated",
  "AgentToolUsed",
  "TodoUpdated",
  "PlanUpdated",
  "Notification",
  "FlagChanged",
  "MonitorUpdated",
  "HighlightPanel",
  "Custom",
  "SnapshotRequired",
  "ChatSessionCompacted",
  "ChatMessageAdded",
  "ChatSessionStatusChanged",
  "MutationRecorded",
  "EventEnriched",
  "ProjectCreated",
  "ProjectDeleted",
  "MigrationApplied",
  "MigrationRolledBack",
  "RunConfigChanged",
  "MemoryUpdated",
  "ProviderHealthChanged",
  "PluginChanged",
  "SettingChanged",
  "SubagentRunChanged",
  "OutputChannelCreated",
  "KnowledgeNoteCreated",
  "KnowledgeNoteUpdated",
  "KnowledgeLinkCreated",
  "DocumentGenerated",
] as const;

export function dispatchProjectUiBridges(env: EnvelopedEvent): void {
  if (typeof window === "undefined") return;
  const kind = env.payload.kind;

  if (kind === "FileChanged") {
    window.dispatchEvent(new CustomEvent("nexus:file:changed", { detail: env.payload }));
    window.dispatchEvent(new CustomEvent("nexus:explorer:refresh", { detail: env.payload }));
  }

  if (kind === "MutationRecorded") {
    window.dispatchEvent(new CustomEvent("nexus:mutations:refresh", { detail: env.payload }));
  }

  if (kind === "SnapshotRequired") {
    useProjectStore.getState().bumpOperationalRefresh(env.ts);
  }
}

export function bumpOperationalRefreshOnConnect(): void {
  useProjectStore.getState().bumpOperationalRefresh(Date.now());
}
