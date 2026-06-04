import { API_BASE, fetchJson } from "./_shared";

export interface QualityFinding {
  id: string;
  filePath: string;
  category: string;
  severity: "high" | "medium" | "low";
  title: string;
  detail: string;
  lineNumber: number | null;
  fixedAt: string | null;
  rule_key?: string;
}

export interface QualityScanResult {
  projectId: string;
  totalFindings: number;
  filesScanned: number;
  bySeverity: Record<string, number>;
  byCategory: Record<string, number>;
}

export async function runQualityScan(projectId: string): Promise<QualityScanResult> {
  const start: { scan_id?: number; status?: string } = await fetchJson(
    `${API_BASE}/api/projects/${projectId}/quality-scan`,
    { method: "POST" },
  );
  if (start.scan_id != null) {
    return pollQualityScanStatus(projectId, start.scan_id);
  }
  return start as unknown as QualityScanResult;
}

async function pollQualityScanStatus(projectId: string, scanId: number): Promise<QualityScanResult> {
  const maxAttempts = 120;
  for (let i = 0; i < maxAttempts; i++) {
    await new Promise((r) => setTimeout(r, 2000));
    const poll: {
      status: string;
      totalFindings?: number;
      filesScanned?: number;
      bySeverity?: Record<string, number>;
      byCategory?: Record<string, number>;
      errorMessage?: string;
    } = await fetchJson(`${API_BASE}/api/projects/${projectId}/quality-scan/${scanId}`);
    if (poll.status === "completed") {
      return {
        projectId,
        totalFindings: poll.totalFindings ?? 0,
        filesScanned: poll.filesScanned ?? 0,
        bySeverity: poll.bySeverity ?? {},
        byCategory: poll.byCategory ?? {},
      };
    }
    if (poll.status === "failed") {
      throw new Error(poll.errorMessage ?? "Scansione fallita");
    }
  }
  throw new Error("Timeout scansione qualita'");
}

export async function getQualityFindings(
  projectId: string,
  opts: { severity?: string; category?: string; limit?: number } = {}
): Promise<{ findings: QualityFinding[]; total: number }> {
  const params = new URLSearchParams();
  if (opts.severity) params.set("severity", opts.severity);
  if (opts.category) params.set("category", opts.category);
  if (opts.limit) params.set("limit", String(opts.limit));
  const qs = params.toString() ? `?${params}` : "";
  return fetchJson(`${API_BASE}/api/projects/${projectId}/quality-findings${qs}`);
}

export async function markFindingFixed(projectId: string, findingId: string): Promise<void> {
  await fetchJson(`${API_BASE}/api/projects/${projectId}/quality-findings/${findingId}/mark-fixed`, { method: "POST" });
}

/**
 * Analizza un singolo file del progetto e restituisce i finding senza toccare il DB.
 * Usato per la verifica post-fix: controlla se un problema è stato effettivamente risolto
 * prima di marcarlo come fixed.
 */
export async function scanProjectFile(
  projectId: string,
  filePath: string
): Promise<{ findings: Omit<QualityFinding, "id" | "fixedAt">[] }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/quality-scan-file`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ file_path: filePath }),
  });
}

/**
 * Legge un intervallo di righe di un file del progetto.
 * Usato per includere il codice contestuale nei messaggi di fix.
 */
export async function getIndexStatus(
  projectId: string
): Promise<{ stale: string[]; staleCount: number; upToDate: number; notIndexed: number; totalFiles: number }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/index-status`);
}

export async function triggerReindexStale(
  projectId: string
): Promise<{ reindexed: number; skipped: number; total: number }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/reindex-stale`, { method: "POST" });
}

export async function readProjectFileLines(
  projectId: string,
  filePath: string,
  startLine: number,
  endLine: number
): Promise<{ lines: string; startLine: number; endLine: number }> {
  const params = new URLSearchParams({
    path: filePath,
    start: String(startLine),
    end: String(endLine),
  });
  return fetchJson(`${API_BASE}/api/projects/${projectId}/file-lines?${params}`);
}

export async function submitDeepReview(projectId: string): Promise<{ jobName: string; jobId: string; fileCount: number; status: string }> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/deep-review`, { method: "POST" });
}

export async function getDeepReviewStatus(projectId: string, jobId: string): Promise<{
  state: string;
  completed: number;
  total: number;
  results?: Array<{
    path: string;
    issues: Array<{ line: number; severity: string; category: string; message: string; suggestion: string }>;
  }>;
}> {
  return fetchJson(`${API_BASE}/api/projects/${projectId}/deep-review/${encodeURIComponent(jobId)}`);
}

export async function markFindingFalsePositive(findingId: number | string, reason?: string, ruleKey?: string): Promise<void> {
  await fetch(`${API_BASE}/api/quality/findings/${findingId}/false-positive`, {
    method: 'POST',
    credentials: "include",
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ reason, rule_key: ruleKey }),
  });
}

export async function getFalsePositiveStats(): Promise<Array<{ rule_key: string; count: number }>> {
  const res = await fetch(`${API_BASE}/api/quality/false-positive-stats`, { credentials: "include" });
  if (!res.ok) return [];
  return res.json();
}
