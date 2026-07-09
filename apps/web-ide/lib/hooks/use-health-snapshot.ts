import { useEffect } from "react";
import type { HealthResponse } from "../api/system";
import { subscribeHealthSnapshot } from "../api/health-monitor";

/** Ascolta lo snapshot health condiviso (health-monitor, regola L). */
export function useHealthSnapshot(onSnapshot: (health: HealthResponse) => void): void {
  useEffect(() => subscribeHealthSnapshot(onSnapshot), [onSnapshot]);
}
