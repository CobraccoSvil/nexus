// Client API per il sistema file_mutations (mig 0349).
// Tutte le chiamate vanno al proxy Next che inoltra a mcp-core sulla 4000.

import { fetchJson } from "./_shared";

const API_BASE = "";

export interface FileMutation {
  id: number;
  project_id: string;
  session_id: string | null;
  user_id: string | null;
  file_path: string;
  tool_name: string;
  op: "created" | "modified" | "deleted" | "reverted";
  before_size: number | null;
  after_size: number | null;
  before_sha256: string | null;
  after_sha256: string | null;
  revertible: boolean;
  reverted_at: string | null;
  reverts_mutation_id: number | null;
  created_at: string;
}

export interface MutationDetail extends Omit<FileMutation, "revertible"> {
  before_content: string | null;
  after_content: string | null;
}

export async function listMutations(
  projectId: string,
  limit = 100,
): Promise<{ mutations: FileMutation[] }> {
  return fetchJson(
    `${API_BASE}/api/projects/${projectId}/mutations?limit=${limit}`,
  );
}

export async function getMutationDetail(
  projectId: string,
  mutationId: number,
): Promise<MutationDetail> {
  return fetchJson(
    `${API_BASE}/api/projects/${projectId}/mutations/${mutationId}`,
  );
}

export interface RevertResult {
  ok: boolean;
  new_mutation_id: number;
  message?: string;
}

export async function revertMutation(
  projectId: string,
  mutationId: number,
  force = false,
): Promise<RevertResult> {
  return fetchJson(
    `${API_BASE}/api/projects/${projectId}/mutations/${mutationId}/revert`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ force }),
    },
  );
}
