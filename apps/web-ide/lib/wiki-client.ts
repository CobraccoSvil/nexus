// Client API unificato per il Knowledge Base (ADR 0017 v2).
// Mappa 1:1 sugli endpoint REST `/api/wiki/*` esposti da mcp-core.
// Riusa il wrapper fetch comune in lib/api/_shared.ts: nessuna duplicazione
// della auth/JWT logic e della gestione errori.

import { fetchJson } from "./api/_shared";

// ─────────────────────────────── Tipi ────────────────────────────────────

export type WikiScope = "meta" | "project";

export type WikiKind =
  | "adr"
  | "note"
  | "runbook"
  | "architecture"
  | "api"
  | "schema"
  | "changelog"
  | "concept"
  | "decision"
  | "other"
  | string;

export type WikiRelType =
  | "relates"
  | "supersedes"
  | "depends_on"
  | "illustrates"
  | "contradicts"
  | "followup"
  | "correction_of"
  | "refines"
  | "duplicate_of"
  | "blocks"
  | "blocked_by"
  | "mentions"
  | "implements"
  | "tests";

export type WikiTripleSource =
  | "wikilink"
  | "semantic"
  | "llm"
  | "user"
  | "agent"
  | "external";

export interface WikiDoc {
  id: string;
  scope: WikiScope;
  project_id: string | null;
  slug: string;
  title: string;
  body_md: string;
  kind: WikiKind;
  intent: string | null;
  tags: string[];
  vault_file_path: string | null;
  edit_lock: "none" | "protected" | "frozen";
  manually_edited: boolean;
  current_version: number;
  auto_generated: boolean;
  public_read: boolean;
  created_at: string;
  updated_at: string;
}

export interface WikiLink {
  from_doc_id: string;
  to_doc_id: string;
  rel_type: WikiRelType;
  confidence: number;
  created_by: "auto" | "user" | "agent" | "llm" | "external";
  evidence: string | null;
  created_at: string;
}

export interface WikiTriple {
  id: string;
  subj_doc_id: string;
  predicate: WikiRelType | string;
  obj_doc_id: string | null;
  obj_text: string | null;
  obj_external: string | null;
  source: WikiTripleSource;
  confidence: number;
  evidence: string | null;
  created_at: string;
}

export interface WikiRevision {
  id: string;
  doc_id: string;
  version_no: number;
  title: string;
  body_md: string;
  body_hash: string;
  tags: string[];
  source: "auto" | "manual" | "import" | "revert" | string;
  author: string | null;
  edit_summary: string | null;
  created_at: string;
}

export interface WikiGraphNode {
  id: string;
  scope: WikiScope;
  project_id: string | null;
  slug: string;
  title: string;
  kind: WikiKind;
  tags: string[];
  auto_generated: boolean;
}

export interface WikiGraphEdge {
  from: string;
  to: string;
  rel_type: WikiRelType | string;
  confidence: number;
  created_by: string;
}

export interface WikiGraphData {
  nodes: WikiGraphNode[];
  edges: WikiGraphEdge[];
  stats?: { nodes_count: number; edges_count: number };
}

export interface WikiSearchHit {
  doc: WikiDoc;
  score: number;
  snippet: string | null;
}

export interface WikiListParams {
  scope: WikiScope;
  project_id?: string;
  kind?: WikiKind;
  tags?: string[];
  q?: string;
  limit?: number;
  offset?: number;
}

// ───────────────────────────── Helper QS ─────────────────────────────────

function qs(params: Record<string, unknown>): string {
  const sp = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v == null) continue;
    if (Array.isArray(v)) {
      for (const item of v) sp.append(k, String(item));
    } else {
      sp.set(k, String(v));
    }
  }
  const s = sp.toString();
  return s ? `?${s}` : "";
}

// ───────────────────────────── Docs CRUD ─────────────────────────────────

export async function listDocs(params: WikiListParams): Promise<{
  items: WikiDoc[];
  total: number;
  limit: number;
  offset: number;
}> {
  return fetchJson(`/api/wiki/docs${qs(params as unknown as Record<string, unknown>)}`);
}

export async function getDoc(id: string): Promise<WikiDoc> {
  return fetchJson(`/api/wiki/docs/${id}`);
}

export async function createDoc(
  doc: Partial<WikiDoc> & { scope: WikiScope; title: string; slug: string; kind: WikiKind },
): Promise<WikiDoc> {
  return fetchJson(`/api/wiki/docs`, {
    method: "POST",
    body: JSON.stringify(doc),
  });
}

export async function patchDoc(
  id: string,
  patch: Partial<WikiDoc>,
  edit_summary?: string,
): Promise<{ doc: WikiDoc; body_changed: boolean; version: number }> {
  return fetchJson(`/api/wiki/docs/${id}`, {
    method: "PATCH",
    body: JSON.stringify({ ...patch, edit_summary }),
  });
}

export async function deleteDoc(id: string): Promise<void> {
  await fetchJson(`/api/wiki/docs/${id}`, { method: "DELETE" });
}

// ───────────────────────────── Revisions ─────────────────────────────────

export async function listRevisions(id: string): Promise<{ items: WikiRevision[]; total: number }> {
  return fetchJson(`/api/wiki/docs/${id}/revisions`);
}

export async function getRevision(id: string, version: number): Promise<WikiRevision> {
  return fetchJson(`/api/wiki/docs/${id}/revisions/${version}`);
}

export async function diffRevisions(
  id: string,
  from: number,
  to: number,
): Promise<{ from: WikiRevision; to: WikiRevision }> {
  return fetchJson(`/api/wiki/docs/${id}/diff?from=${from}&to=${to}`);
}

export async function restoreRevision(
  id: string,
  version: number,
): Promise<{ ok: boolean; restored_from: number; version: number; doc: WikiDoc }> {
  return fetchJson(`/api/wiki/docs/${id}/restore`, {
    method: "POST",
    body: JSON.stringify({ version }),
  });
}

// ───────────────────────────── Links / Triples ───────────────────────────

export interface WikiLinksResponse {
  outbound: Array<WikiLink & { to_doc: WikiDoc }>;
  inbound: Array<WikiLink & { from_doc: WikiDoc }>;
}

export async function getDocLinks(id: string): Promise<WikiLinksResponse> {
  return fetchJson(`/api/wiki/docs/${id}/links`);
}

export interface WikiDocTriplesResponse {
  outbound: WikiTriple[];
  inbound: WikiTriple[];
  totals: { outbound: number; inbound: number };
}

export async function getDocTriples(id: string): Promise<WikiDocTriplesResponse> {
  return fetchJson(`/api/wiki/docs/${id}/triples`);
}

export interface WikiTripleFilters {
  scope?: WikiScope;
  project_id?: string;
  predicate?: string;
  source?: WikiTripleSource;
  min_confidence?: number;
  q?: string;
  limit?: number;
  offset?: number;
}

export async function listTriples(
  filters: WikiTripleFilters = {},
): Promise<{ items: WikiTriple[]; total: number; limit: number; offset: number }> {
  return fetchJson(`/api/wiki/triples${qs(filters as Record<string, unknown>)}`);
}

// ───────────────────────────── Graph ─────────────────────────────────────

export interface WikiGraphParams {
  min_confidence?: number;
  predicate?: string;
  center_doc_id?: string;
  max_hops?: number;
  max_nodes?: number;
  hide_auto_links?: boolean;
}

export async function getGraph(
  scope: WikiScope,
  project_id?: string,
  opts: WikiGraphParams = {},
): Promise<WikiGraphData> {
  return fetchJson(
    `/api/wiki/graph${qs({ scope, project_id, ...opts } as Record<string, unknown>)}`,
  );
}

// ───────────────────────────── Search ────────────────────────────────────

export interface WikiSearchParams {
  scope?: WikiScope;
  project_id?: string;
  kind?: WikiKind;
  limit?: number;
}

export async function search(
  q: string,
  opts: WikiSearchParams = {},
): Promise<{ results: WikiSearchHit[] }> {
  return fetchJson(`/api/wiki/search${qs({ q, ...opts } as Record<string, unknown>)}`);
}

// ─────────────────────────── Maintenance ─────────────────────────────────

export interface WikiMaintenanceResponse {
  ok: boolean;
  started?: boolean;
  notes_processed?: number;
  wikilinks_created?: number;
  semantic_links_created?: number;
  triples_extracted?: number;
  docs_reingested?: number;
  warnings?: string[];
}

export async function recomputeLinks(params: {
  scope: WikiScope;
  project_id?: string;
  doc_id?: string;
  wait?: boolean;
}): Promise<WikiMaintenanceResponse> {
  return fetchJson(`/api/wiki/recompute-links`, {
    method: "POST",
    body: JSON.stringify(params),
  });
}

export async function extractTriples(params: {
  scope: WikiScope;
  project_id?: string;
  doc_id?: string;
  wait?: boolean;
  override_cap?: boolean;
}): Promise<WikiMaintenanceResponse> {
  return fetchJson(`/api/wiki/extract-triples`, {
    method: "POST",
    body: JSON.stringify(params),
  });
}

export async function reingest(params: {
  scope: WikiScope;
  project_id?: string;
  wait?: boolean;
}): Promise<WikiMaintenanceResponse> {
  return fetchJson(`/api/wiki/reingest`, {
    method: "POST",
    body: JSON.stringify(params),
  });
}
