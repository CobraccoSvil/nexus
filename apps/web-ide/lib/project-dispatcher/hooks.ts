// Hook React per consumare il dispatcher.

import { useEffect } from "react";
import { connectDispatcher, disconnectDispatcher } from "./connection";

/**
 * Hook unico per la pagina IDE: aggancia il dispatcher al progetto attivo.
 * Su cambio progetto, riconnette automaticamente. Su unmount, disconnette.
 */
export function useProjectDispatcher(projectId: string | null | undefined): void {
  useEffect(() => {
    if (!projectId) return;
    void connectDispatcher(projectId);
    return () => {
      // Non disconnettere su semplice change di props: la connessione viene
      // ri-aperta se il projectId cambia, oppure smontata dal componente padre.
    };
  }, [projectId]);

  useEffect(() => {
    return () => {
      disconnectDispatcher();
    };
  }, []);
}

// Re-export selettori comuni
export { useProjectStore } from "./store";
export {
  selectChatLastCompact,
  selectChatLastMessage,
  selectChatStatus,
  selectConnection,
  selectDatabaseQueries,
  selectDbConfigUpdatedAt,
  selectEnrichmentByEventId,
  selectFilesRecent,
  selectFindingsUpdate,
  selectOperationalRefreshAt,
  selectProblemsRefreshAt,
  selectServicesRefreshAt,
  selectMutationsRefreshAt,
  selectServiceLogsRecent,
  selectFlags,
  selectGitStatus,
  selectHighlight,
  selectKnowledgeChangedAt,
  selectMemoryChangedAt,
  selectMigrationsChangedAt,
  selectMonitors,
  selectMutationsRecent,
  selectOutputChannelsChangedAt,
  selectPlaywrightRuns,
  selectPlaywrightConfigChangedAt,
  selectPluginsChangedAt,
  selectPorts,
  selectProblemsBadge,
  selectProviderHealthChangedAt,
  selectQualityScan,
  selectRunConfigsChangedAt,
  selectServicesMap,
  selectSettingsChangedAt,
  selectSubagentRunsChangedAt,
  selectToasts,
  selectTodoStatuses,
  subscribeAll,
} from "./store";
export { useEventOfKind, useProjectEvents } from "./use-events";
export { useOperationalRefresh } from "./use-operational-data";
export type { EventFilter, EventHandler } from "./use-events";
