import { fetchJson } from "./_shared";

// ── Knowledge Base ──────────────────────────────────────────────────

export interface KnowledgeNote {
  id: string;
  projectId: string;
  sourceMessageId: string | null;
  sourceRunId: string | null;
  intent: string | null;
  title: string;
  bodyMd: string;
  status: string;
  tags: string[];
  filePaths: string[];
  vaultFilePath: string | null;
  accessCount: number;
  createdAt: string;
  updatedAt: string;
  lastAccessedAt: string | null;
  // detail only
  outgoing?: KnowledgeLink[];
  backlinks?: KnowledgeLink[];
}

export interface KnowledgeLink {
  linkId: string;
  fromNoteId?: string;
  toNoteId?: string;
  fromTitle?: string;
  toTitle?: string;
  relType: string;
  createdBy: string;
  confidence: number;
}

export interface KnowledgeTag {
  tag: string;
  noteCount: number;
  lastUsedAt: string;
}

export interface SimilarHit {
  noteId: string;
  title: string;
  intent: string | null;
  status: string;
  score: number;
  createdAt: string;
  // M14.4: stato di implementazione della richiesta gia presente in KB.
  implemented?: boolean;
  runStatus?: string | null;
  runCompletedAt?: string | null;
}

export async function listKnowledgeNotes(
  projectId: string,
  params?: { status?: string; intent?: string; tag?: string; q?: string; limit?: number; offset?: number },
): Promise<{ notes: KnowledgeNote[]; total: number; limit: number; offset: number }> {
  const sp = new URLSearchParams();
  if (params?.status) sp.set("status", params.status);
  if (params?.intent) sp.set("intent", params.intent);
  if (params?.tag) sp.set("tag", params.tag);
  if (params?.q) sp.set("q", params.q);
  if (params?.limit) sp.set("limit", String(params.limit));
  if (params?.offset) sp.set("offset", String(params.offset));
  const qs = sp.toString();
  return fetchJson(`/api/projects/${projectId}/knowledge/notes${qs ? `?${qs}` : ""}`);
}

export async function getKnowledgeNote(projectId: string, noteId: string): Promise<KnowledgeNote> {
  return fetchJson(`/api/projects/${projectId}/knowledge/notes/${noteId}`);
}

export async function patchKnowledgeNote(
  projectId: string,
  noteId: string,
  body: { title?: string; body_md?: string; tags?: string[]; status?: string },
): Promise<{ ok: boolean; noteId: string }> {
  return fetchJson(`/api/projects/${projectId}/knowledge/notes/${noteId}`, {
    method: "PATCH",
    body: JSON.stringify(body),
  });
}

export async function deleteKnowledgeNote(
  projectId: string,
  noteId: string,
): Promise<{ ok: boolean; deleted: string }> {
  return fetchJson(`/api/projects/${projectId}/knowledge/notes/${noteId}`, {
    method: "DELETE",
  });
}

export async function findSimilarKnowledge(
  projectId: string,
  text: string,
  signal?: AbortSignal,
): Promise<{ hits: SimilarHit[] }> {
  return fetchJson(`/api/projects/${projectId}/knowledge/similar`, {
    method: "POST",
    body: JSON.stringify({ text }),
    signal,
  });
}

export async function createKnowledgeLink(
  projectId: string,
  body: { from_note_id: string; to_note_id: string; rel_type: string },
): Promise<{ linkId: string }> {
  return fetchJson(`/api/projects/${projectId}/knowledge/links`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export async function deleteKnowledgeLink(projectId: string, linkId: string): Promise<{ ok: boolean }> {
  return fetchJson(`/api/projects/${projectId}/knowledge/links/${linkId}`, { method: "DELETE" });
}

export async function listKnowledgeTags(projectId: string): Promise<{ tags: KnowledgeTag[] }> {
  return fetchJson(`/api/projects/${projectId}/knowledge/tags`);
}

// ── Knowledge graph (Cytoscape data) ────────────────────────────────────

export interface KnowledgeGraphNode {
  id: string;
  title: string;
  intent: string | null;
  status: string;
  tags: string[];
  access_count: number;
  updated_at: string | null;
}

export interface KnowledgeGraphEdge {
  id: string;
  from: string;
  to: string;
  rel_type: string;
  created_by: string;
  confidence: number;
}

export interface KnowledgeGraphData {
  nodes: KnowledgeGraphNode[];
  edges: KnowledgeGraphEdge[];
  stats: { nodes_count: number; edges_count: number };
}

export async function getKnowledgeGraph(
  projectId: string,
  params?: { status?: string; min_confidence?: number },
): Promise<KnowledgeGraphData> {
  const sp = new URLSearchParams();
  if (params?.status) sp.set("status", params.status);
  if (params?.min_confidence != null) sp.set("min_confidence", String(params.min_confidence));
  const qs = sp.toString();
  return fetchJson(`/api/projects/${projectId}/knowledge/graph${qs ? `?${qs}` : ""}`);
}

export async function recomputeKnowledgeLinks(
  projectId: string,
): Promise<{ ok: boolean; notes_processed: number; links_created: number }> {
  return fetchJson(`/api/projects/${projectId}/knowledge/recompute-links`, {
    method: "POST",
    body: "{}",
  });
}

// W2 code-wiki: avvia la generazione della documentazione AI per-file.
// La generazione gira in background; le note code_doc compaiono man mano.
export async function generateCodeWiki(
  projectId: string,
): Promise<{ ok: boolean; started: boolean }> {
  return fetchJson(`/api/projects/${projectId}/knowledge/code-wiki/generate`, {
    method: "POST",
    body: "{}",
  });
}

export async function rebuildKnowledge(
  projectId: string,
  opts?: { reset?: boolean },
): Promise<{
  ok: boolean;
  reset: boolean;
  messages_total: number;
  notes_created: number;
  skipped_short: number;
  linked_notes: number;
  links_created: number;
}> {
  return fetchJson(`/api/projects/${projectId}/knowledge/rebuild`, {
    method: "POST",
    body: JSON.stringify({ reset: opts?.reset ?? false }),
  });
}

export async function extractFunctionalSpecs(
  projectId: string,
  opts?: { limit?: number; include_files?: boolean; files_limit?: number },
): Promise<{
  ok: boolean;
  messages_scanned: number;
  messages_skipped_short: number;
  messages_with_specs: number;
  files_scanned: number;
  files_skipped_short: number;
  files_with_specs: number;
  specs_extracted: number;
  specs_applied: number;
  llm_errors: number;
  linked_notes: number;
  links_created: number;
}> {
  return fetchJson(`/api/projects/${projectId}/knowledge/extract-functional`, {
    method: "POST",
    body: JSON.stringify({
      limit: opts?.limit ?? 50,
      include_files: opts?.include_files ?? true,
      files_limit: opts?.files_limit ?? 80,
    }),
  });
}

/**
 * Endpoint unificato: inizializza o aggiorna l'intera KB del progetto in
 * un solo colpo (resiliente). Sostituisce il flusso a tre tasti.
 *
 * Pipeline interna:
 *   1. FunctionalSpecAgent (chat + file `.md`/sorgenti) → note kind=functional
 *   2. 3 generator (technical/functional/test)
 *   3. Rebuild idempotente da chat_messages user
 *   4. Ricalcolo link automatici
 */
export async function initOrRefreshKnowledge(
  projectId: string,
  opts?: { reset?: boolean; chat_limit?: number; files_limit?: number },
): Promise<{
  ok: boolean;
  reset: boolean;
  deleted_notes: number;
  functional_agent: {
    messages_scanned?: number;
    messages_with_specs?: number;
    files_scanned?: number;
    files_with_specs?: number;
    specs_extracted?: number;
    specs_applied?: number;
    llm_errors?: number;
  };
  generators: { notes_generated?: number; notes_applied?: number };
  rebuild_from_chat: { messages_total?: number; notes_created?: number };
  links: { notes_processed?: number; links_created?: number };
  warnings: string[];
}> {
  return fetchJson(`/api/projects/${projectId}/knowledge/init-or-refresh`, {
    method: "POST",
    body: JSON.stringify({
      reset: opts?.reset ?? false,
      chat_limit: opts?.chat_limit ?? 100,
      files_limit: opts?.files_limit ?? 80,
    }),
  });
}

export async function createKnowledgeNoteManual(
  projectId: string,
  body: { title: string; body_md: string; intent?: string; tags?: string[]; file_paths?: string[] },
): Promise<{ ok: boolean; note_id: string; intent: string }> {
  return fetchJson(`/api/projects/${projectId}/knowledge/notes/manual`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}

// ── Obsidian vault config (per progetto) ────────────────────────────────

export async function getObsidianVaultName(projectId: string): Promise<{ obsidian_vault_name: string }> {
  return fetchJson(`/api/projects/${projectId}/knowledge/obsidian-vault`);
}

export async function putObsidianVaultName(
  projectId: string,
  obsidian_vault_name: string,
): Promise<{ ok: boolean; obsidian_vault_name: string }> {
  return fetchJson(`/api/projects/${projectId}/knowledge/obsidian-vault`, {
    method: "PUT",
    body: JSON.stringify({ obsidian_vault_name }),
  });
}

// ── Wiki editing + versioning per knowledge notes (Fase 2/3 wiki unification) ──

export async function listKnowledgeNoteRevisions(
  projectId: string,
  noteId: string,
): Promise<{ items: WikiRevision[]; total: number }> {
  return fetchJson(
    `/api/projects/${projectId}/knowledge/notes/${noteId}/revisions`,
  );
}

export async function getKnowledgeNoteRevision(
  projectId: string,
  noteId: string,
  version: number,
): Promise<WikiRevision> {
  return fetchJson(
    `/api/projects/${projectId}/knowledge/notes/${noteId}/revisions/${version}`,
  );
}

export async function getKnowledgeNoteDiff(
  projectId: string,
  noteId: string,
  from: number,
  to: number,
): Promise<{ from: WikiRevision; to: WikiRevision }> {
  return fetchJson(
    `/api/projects/${projectId}/knowledge/notes/${noteId}/diff?from=${from}&to=${to}`,
  );
}

export async function restoreKnowledgeNoteRevision(
  projectId: string,
  noteId: string,
  version: number,
): Promise<{ ok: boolean; restored_from: number; version: number }> {
  return fetchJson(
    `/api/projects/${projectId}/knowledge/notes/${noteId}/restore`,
    { method: "POST", body: JSON.stringify({ version }) },
  );
}

// Tipo revisione wiki, condiviso tra knowledge notes e meta-docs.
export interface WikiRevision {
  version_no: number;
  title: string;
  source: "auto" | "manual" | "import" | "revert" | string;
  author?: string | null;
  edit_summary?: string | null;
  body_bytes?: number;
  body_md?: string;
  tags?: string[];
  created_at: string;
}
