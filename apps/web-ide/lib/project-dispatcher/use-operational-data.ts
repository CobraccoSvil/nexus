// Hook unico: trigger SSE -> refetch viste operative (regola L / P2 audit pannelli).

import { useEffect, useRef } from "react";
import { useProjectStore, selectOperationalRefreshAt } from "./store";

/**
 * Consolida i trigger di invalidazione operativa in un solo posto.
 * ide-shell passa `onRefresh` (= refreshOperationalViews).
 *
 * Reconnect: `connection.ts` chiama `bumpOperationalRefreshOnConnect` su
 * EventSource `onopen` — un solo refetch, niente doppio trigger su status.
 */
export function useOperationalRefresh(
  projectId: string | null | undefined,
  onRefresh: (projectId: string) => void,
): void {
  const operationalRefreshAt = useProjectStore(selectOperationalRefreshAt);
  const onRefreshRef = useRef(onRefresh);
  onRefreshRef.current = onRefresh;

  useEffect(() => {
    if (!projectId || operationalRefreshAt === 0) return;
    void onRefreshRef.current(projectId);
  }, [projectId, operationalRefreshAt]);
}
