// This file is used BOTH server-side (Server Components) and client-side.
// Server-side: Node.js fetch needs absolute URL → use BACKEND_URL or localhost:4000
// Client-side: relative URLs work via Next.js rewrites
const API_BASE =
  typeof window === "undefined"
    ? process.env.BACKEND_URL || "http://localhost:4000"
    : "";

export interface DashboardSnapshot {
  tokenUsage: {
    consumed: number;
    saved: number;
  };
  quality: {
    findings: number;
    shadowDbStatus: string;
  };
  health: {
    database: boolean;
    redis: boolean;
    neural_core: boolean;
  };
  runs: {
    total: number;
    activeJobs: number;
  };
}

export async function getDashboardSnapshot(): Promise<DashboardSnapshot> {
  try {
    const [dashRes, health] = await Promise.all([
      fetch(`${API_BASE}/api/dashboard`, { credentials: "include", cache: "no-store" }).then((r) => r.json()),
      fetch(`${API_BASE}/api/health`, { credentials: "include", cache: "no-store" }).then((r) => r.json()),
    ]);

    return {
      tokenUsage: {
        consumed: dashRes.tokenUsage?.consumed ?? dashRes.tokens_consumed ?? 0,
        saved: dashRes.tokenUsage?.saved ?? dashRes.tokens_saved ?? 0,
      },
      quality: {
        findings: dashRes.quality?.findings ?? dashRes.quality_findings ?? 0,
        shadowDbStatus: dashRes.quality?.shadowDbStatus ?? "idle",
      },
      health: health.components ?? { database: false, redis: false, neural_core: false },
      runs: {
        total: dashRes.total_runs ?? 0,
        activeJobs: dashRes.active_jobs ?? 0,
      },
    };
  } catch {
    return {
      tokenUsage: { consumed: 0, saved: 0 },
      quality: { findings: 0, shadowDbStatus: "offline" },
      health: { database: false, redis: false, neural_core: false },
      runs: { total: 0, activeJobs: 0 },
    };
  }
}
