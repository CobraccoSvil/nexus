import { fetchJson } from "./_shared";
import type { KnowledgeGraphEdge, WikiRevision } from "./knowledge";

// ── Meta-docs (documentazione del meta-progetto Nexus) ──────────────────

export type MetaDocKind =
  | "architecture"
  | "adr"
  | "api"
  | "schema"
  | "runbook"
  | "changelog"
  | "decision"
  | "other";

export interface MetaDocSummary {
  id: string;
  kind: MetaDocKind;
  title: string;
  slug: string;
  vault_file_path: string;
  tags: string[];
  auto_generated: boolean;
  updated_at: string;
}

export interface MetaDocDetail extends MetaDocSummary {
  body_md: string;
  vault_file_hash: string;
  source_commit: string | null;
  source_files: string[];
  created_at: string;
  outgoing_links: Array<{
    to_or_from_id: string | null;
    rel_type: string;
    created_by: string;
    confidence: number;
    title: string;
    slug: string;
  }>;
  incoming_links: Array<{
    to_or_from_id: string | null;
    rel_type: string;
    created_by: string;
    confidence: number;
    title: string;
    slug: string;
  }>;
}

export async function listMetaDocs(params?: {
  kind?: MetaDocKind;
  q?: string;
  limit?: number;
  offset?: number;
}): Promise<{ items: MetaDocSummary[]; total: number; limit: number; offset: number }> {
  const sp = new URLSearchParams();
  if (params?.kind) sp.set("kind", params.kind);
  if (params?.q) sp.set("q", params.q);
  if (params?.limit != null) sp.set("limit", String(params.limit));
  if (params?.offset != null) sp.set("offset", String(params.offset));
  const qs = sp.toString();
  return fetchJson(`/api/meta-docs/list${qs ? `?${qs}` : ""}`);
}

export async function getMetaDoc(id: string): Promise<MetaDocDetail> {
  return fetchJson(`/api/meta-docs/${id}`);
}

export async function triggerMetaDocsRefresh(): Promise<{
  status: string;
  generated?: number;
  applied?: number;
  skipped?: number;
  errors?: string[];
}> {
  return fetchJson(`/api/meta-docs/refresh-all`, { method: "POST", body: "{}" });
}

interface MetaDocsGraphNode {
  id: string;
  kind: MetaDocKind;
  title: string;
  slug: string;
  tags: string[];
  auto_generated: boolean;
  updated_at: string | null;
}

export interface MetaDocsGraphData {
  nodes: MetaDocsGraphNode[];
  edges: KnowledgeGraphEdge[];
  stats: { nodes_count: number; edges_count: number };
}

export async function getMetaDocsGraph(params?: { kind?: MetaDocKind }): Promise<MetaDocsGraphData> {
  const sp = new URLSearchParams();
  if (params?.kind) sp.set("kind", params.kind);
  const qs = sp.toString();
  return fetchJson(`/api/meta-docs/graph${qs ? `?${qs}` : ""}`);
}

export async function recomputeMetaDocsLinks(): Promise<{
  ok: boolean;
  notes_processed: number;
  wikilinks_created: number;
  wikilinks_unresolved: number;
  semantic_links_created?: number;
}> {
  return fetchJson(`/api/meta-docs/recompute-links`, { method: "POST", body: "{}" });
}

// ── Wiki editing + versioning (Fase 2/3 wiki unification) ──────────────

export async function patchMetaDoc(
  id: string,
  body: { title?: string; body_md?: string; tags?: string[] },
): Promise<{ ok: boolean; id: string; version: number; body_changed: boolean }> {
  return fetchJson(`/api/meta-docs/${id}`, {
    method: "PATCH",
    body: JSON.stringify(body),
  });
}

export async function listMetaDocRevisions(
  id: string,
): Promise<{ items: WikiRevision[]; total: number }> {
  return fetchJson(`/api/meta-docs/${id}/revisions`);
}

export async function getMetaDocRevision(
  id: string,
  version: number,
): Promise<WikiRevision> {
  return fetchJson(`/api/meta-docs/${id}/revisions/${version}`);
}

export async function restoreMetaDocRevision(
  id: string,
  version: number,
): Promise<{ ok: boolean; restored_from: number; version: number }> {
  return fetchJson(`/api/meta-docs/${id}/restore`, {
    method: "POST",
    body: JSON.stringify({ version }),
  });
}
