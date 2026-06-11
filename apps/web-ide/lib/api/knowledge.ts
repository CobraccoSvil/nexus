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

interface KnowledgeLink {
  linkId: string;
  fromNoteId?: string;
  toNoteId?: string;
  fromTitle?: string;
  toTitle?: string;
  relType: string;
  createdBy: string;
  confidence: number;
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

// ── Knowledge graph (Cytoscape data) ────────────────────────────────────

interface KnowledgeGraphNode {
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
