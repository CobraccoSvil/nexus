import type { PluginToolPolicy } from "../../../lib/api-client";

export type ManagerTab = "installed" | "catalog" | "policy" | "legacy";

export interface PolicyDraft {
  mode: PluginToolPolicy["mode"];
  tools: string;
  blockedTools: string;
}

export interface PluginTestStatus {
  success: boolean;
  toolCount: number;
  error?: string;
  at: string;
}
