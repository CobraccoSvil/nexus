"use client";

/**
 * Admin / Doc Nexus
 *
 * Pagina di gestione della **documentazione del meta-progetto Nexus**:
 * architettura, ADR, runbook, schema, changelog auto-generato, decisioni
 * estratte da chat. Tutto vive sotto `docs/.nexus-vault/` (filesystem)
 * sincronizzato con la tabella `nexus_meta_docs` (DB).
 *
 * Tre viste:
 *   1. Lista note (filtri kind, search, pulsante Refresh manuale)
 *   2. Grafo (Cytoscape modale fullscreen)
 *   3. Deep-link Obsidian (apre il vault Obsidian dell'utente se installato)
 *
 * NB: questa pagina e' SEPARATA dal Knowledge panel del progetto. Lo
 * scope qui e' la doc di Nexus stesso (sviluppatori/admin Nexus).
 */

import { useEffect, useState, useCallback } from "react";
import {
  listMetaDocs,
  getMetaDoc,
  triggerMetaDocsRefresh,
  recomputeMetaDocsLinks,
  type MetaDocSummary,
  type MetaDocDetail,
  type MetaDocKind,
} from "../../../lib/api-client";
import { MarkdownBlock } from "../../../components/chat/markdown-renderer";
import { KnowledgeGraph } from "../../../components/knowledge/knowledge-graph";
import { useThemeColors } from "../../../lib/theme";

const META_VAULT_NAME_KEY = "nexus.meta_docs.obsidian_vault_name";

const KIND_LABELS: Record<MetaDocKind, string> = {
  architecture: "Architettura",
  adr: "ADR",
  api: "API",
  schema: "Schema",
  runbook: "Runbook",
  changelog: "Changelog",
  decision: "Decisioni",
  other: "Altro",
};

const KIND_COLORS: Record<MetaDocKind, string> = {
  architecture: "#6366f1",
  adr: "#dc2626",
  api: "#0ea5e9",
  schema: "#7c3aed",
  runbook: "#16a34a",
  changelog: "#f59e0b",
  decision: "#db2777",
  other: "#737373",
};

export default function NexusDocsAdminPage() {
  const tc = useThemeColors();
  const [items, setItems] = useState<MetaDocSummary[]>([]);
  const [total, setTotal] = useState(0);
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [kindFilter, setKindFilter] = useState<MetaDocKind | "">("");
  const [q, setQ] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [selected, setSelected] = useState<MetaDocDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [graphOpen, setGraphOpen] = useState(false);
  const [vaultName, setVaultName] = useState("");
  const [showVaultSettings, setShowVaultSettings] = useState(false);
  const [recomputingLinks, setRecomputingLinks] = useState(false);
  const [recomputeMsg, setRecomputeMsg] = useState<string | null>(null);

  useEffect(() => {
    try {
      setVaultName(localStorage.getItem(META_VAULT_NAME_KEY) ?? "");
    } catch {
      // ignore
    }
  }, []);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await listMetaDocs({
        kind: kindFilter || undefined,
        q: q.trim() || undefined,
        limit: 100,
      });
      setItems(res.items);
      setTotal(res.total);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [kindFilter, q]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (!selectedId) {
      setSelected(null);
      return;
    }
    getMetaDoc(selectedId)
      .then(setSelected)
      .catch((e) => setError(String(e)));
  }, [selectedId]);

  const handleRefresh = async () => {
    setRefreshing(true);
    setError(null);
    try {
      const r = await triggerMetaDocsRefresh();
      await load();
      if (r.applied != null) {
        setError(`Refresh completato. Generate: ${r.generated ?? 0}, applicate: ${r.applied}, saltate: ${r.skipped ?? 0}`);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setRefreshing(false);
    }
  };

  const handleRecomputeLinks = async () => {
    setRecomputingLinks(true);
    setRecomputeMsg(null);
    try {
      const r = await recomputeMetaDocsLinks();
      setRecomputeMsg(
        `${r.wikilinks_created} link creati su ${r.notes_processed} note. ${r.wikilinks_unresolved} wikilink non risolti.`,
      );
    } catch (e) {
      setRecomputeMsg("Errore: " + (e instanceof Error ? e.message : String(e)));
    } finally {
      setRecomputingLinks(false);
    }
  };

  const saveVaultName = (name: string) => {
    setVaultName(name);
    try {
      if (name) {
        localStorage.setItem(META_VAULT_NAME_KEY, name);
      } else {
        localStorage.removeItem(META_VAULT_NAME_KEY);
      }
    } catch {
      // ignore
    }
  };

  const openInObsidian = (vaultFilePath?: string) => {
    let name = vaultName.trim();
    if (!name) {
      setShowVaultSettings(true);
      return;
    }
    const fileParam = vaultFilePath
      ? `&file=${encodeURIComponent(vaultFilePath.replace(/\.md$/, ""))}`
      : "";
    window.location.href = `obsidian://open?vault=${encodeURIComponent(name)}${fileParam}`;
  };

  return (
    <div style={{ padding: 24, color: tc.text, maxWidth: 1400, margin: "0 auto" }}>
      <header style={{ marginBottom: 24 }}>
        <h1 style={{ fontSize: 24, fontWeight: 700, margin: "0 0 6px" }}>Doc Nexus (meta-vault)</h1>
        <p style={{ fontSize: 13, color: tc.textMuted, margin: 0 }}>
          Documentazione del meta-progetto Nexus: architettura, ADR, runbook, schema DB, changelog
          auto-generato, decisioni estratte da chat. Vive su <code>docs/.nexus-vault/</code> ed e'
          aperto come vault Obsidian.
        </p>
      </header>

      {/* Azioni globali */}
      <div
        style={{
          display: "flex",
          gap: 8,
          marginBottom: 16,
          flexWrap: "wrap",
          alignItems: "center",
        }}
      >
        <button
          onClick={handleRefresh}
          disabled={refreshing}
          style={{
            padding: "8px 14px",
            fontSize: 13,
            fontWeight: 600,
            background: refreshing ? tc.bgHover : tc.accent,
            color: refreshing ? tc.textMuted : "#fff",
            border: "none",
            borderRadius: 8,
            cursor: refreshing ? "default" : "pointer",
          }}
        >
          {refreshing ? "Refresh in corso..." : "Rigenera tutta la doc"}
        </button>
        <button
          onClick={() => setGraphOpen(true)}
          style={{
            padding: "8px 14px",
            fontSize: 13,
            fontWeight: 600,
            background: "#171717",
            color: "#fff",
            border: "none",
            borderRadius: 8,
            cursor: "pointer",
          }}
        >
          Mostra grafo
        </button>
        <button
          onClick={handleRecomputeLinks}
          disabled={recomputingLinks}
          title="Parsa i wikilink [[...]] nei body delle note e crea le relazioni nel DB"
          style={{
            padding: "8px 14px",
            fontSize: 13,
            fontWeight: 600,
            background: recomputingLinks ? tc.bgHover : "#0ea5e9",
            color: "#fff",
            border: "none",
            borderRadius: 8,
            cursor: recomputingLinks ? "default" : "pointer",
          }}
        >
          {recomputingLinks ? "Calcolo..." : "Ricalcola link"}
        </button>
        <button
          onClick={() => openInObsidian()}
          title={vaultName ? `Vault: ${vaultName}` : "Configura il nome del vault Obsidian"}
          style={{
            padding: "8px 14px",
            fontSize: 13,
            fontWeight: 600,
            background: "#7c3aed",
            color: "#fff",
            border: "none",
            borderRadius: 8,
            cursor: "pointer",
          }}
        >
          Apri in Obsidian
        </button>
        <button
          onClick={() => setShowVaultSettings((v) => !v)}
          style={{
            padding: "8px 14px",
            fontSize: 12,
            background: "transparent",
            color: tc.textMuted,
            border: `1px solid ${tc.border}`,
            borderRadius: 8,
            cursor: "pointer",
          }}
        >
          {showVaultSettings ? "Nascondi setup vault" : "Setup vault Obsidian"}
        </button>
        <div style={{ marginLeft: "auto", fontSize: 12, color: tc.textMuted }}>
          {total} doc nel vault
        </div>
      </div>

      {showVaultSettings && (
        <div
          style={{
            padding: 12,
            background: tc.bgCard,
            border: `1px solid ${tc.border}`,
            borderRadius: 8,
            marginBottom: 16,
            fontSize: 13,
          }}
        >
          <p style={{ margin: "0 0 8px", fontWeight: 600 }}>Configura il vault Obsidian</p>
          <ol style={{ margin: "0 0 8px", paddingLeft: 20, fontSize: 12, lineHeight: 1.6 }}>
            <li>Apri Obsidian</li>
            <li>
              File &rarr; Open vault &rarr; Open folder as vault &rarr; seleziona la cartella{" "}
              <code style={{ background: tc.bgHover, padding: "1px 4px", borderRadius: 3 }}>
                /home/administrator/ideai/docs/.nexus-vault
              </code>{" "}
              (o l'equivalente sul tuo sistema)
            </li>
            <li>Inserisci qui sotto il nome del vault scelto in Obsidian:</li>
          </ol>
          <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
            <input
              value={vaultName}
              onChange={(e) => setVaultName(e.target.value)}
              placeholder='Es. "Nexus-MetaVault"'
              style={{
                flex: 1,
                padding: "6px 10px",
                fontSize: 13,
                border: `1px solid ${tc.border}`,
                borderRadius: 6,
                background: tc.bgInput,
                color: tc.text,
              }}
            />
            <button
              onClick={() => {
                saveVaultName(vaultName.trim());
                setShowVaultSettings(false);
              }}
              style={{
                padding: "6px 14px",
                fontSize: 12,
                fontWeight: 600,
                background: tc.accent,
                color: "#fff",
                border: "none",
                borderRadius: 6,
                cursor: "pointer",
              }}
            >
              Salva
            </button>
          </div>
          <p style={{ margin: "8px 0 0", fontSize: 11, color: tc.textMuted }}>
            Il nome viene salvato in <code>localStorage</code> del browser (per-utente, per-browser).
          </p>
        </div>
      )}

      {/* Filtri kind */}
      <div style={{ display: "flex", gap: 6, marginBottom: 12, flexWrap: "wrap" }}>
        <button
          onClick={() => setKindFilter("")}
          style={{
            padding: "4px 10px",
            fontSize: 11,
            border: `1px solid ${kindFilter === "" ? tc.accent : tc.border}`,
            background: kindFilter === "" ? tc.accent : "transparent",
            color: kindFilter === "" ? "#fff" : tc.textMuted,
            borderRadius: 4,
            cursor: "pointer",
          }}
        >
          Tutto ({total})
        </button>
        {(Object.keys(KIND_LABELS) as MetaDocKind[]).map((k) => (
          <button
            key={k}
            onClick={() => setKindFilter(k)}
            style={{
              padding: "4px 10px",
              fontSize: 11,
              border: `1px solid ${kindFilter === k ? KIND_COLORS[k] : tc.border}`,
              background: kindFilter === k ? KIND_COLORS[k] : "transparent",
              color: kindFilter === k ? "#fff" : KIND_COLORS[k],
              borderRadius: 4,
              cursor: "pointer",
              whiteSpace: "nowrap",
            }}
          >
            {KIND_LABELS[k]}
          </button>
        ))}
      </div>

      {/* Search */}
      <div style={{ marginBottom: 12 }}>
        <input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && load()}
          placeholder="Cerca nel meta-vault (full-text title + body)..."
          style={{
            width: "100%",
            padding: "8px 12px",
            fontSize: 13,
            border: `1px solid ${tc.border}`,
            borderRadius: 8,
            background: tc.bgInput,
            color: tc.text,
            boxSizing: "border-box",
          }}
        />
      </div>

      {error && (
        <div
          style={{
            marginBottom: 12,
            padding: 10,
            background: tc.bgCard,
            color: error.includes("Refresh completato") ? tc.accent : tc.error,
            border: `1px solid ${error.includes("Refresh completato") ? tc.accent : tc.error}`,
            borderRadius: 6,
            fontSize: 12,
          }}
        >
          {error}
        </div>
      )}

      {/* Vista split: lista (sx) + dettaglio (dx) */}
      <div style={{ display: "flex", gap: 16, minHeight: 480 }}>
        <div
          style={{
            flex: "0 0 380px",
            display: "flex",
            flexDirection: "column",
            gap: 4,
            overflowY: "auto",
            maxHeight: "calc(100vh - 360px)",
          }}
        >
          {loading && items.length === 0 && (
            <div style={{ padding: 16, color: tc.textMuted, fontSize: 13 }}>Caricamento...</div>
          )}
          {!loading && items.length === 0 && (
            <div style={{ padding: 16, color: tc.textMuted, fontSize: 13 }}>
              Nessuna doc nel vault. Click su "Rigenera tutta la doc".
            </div>
          )}
          {items.map((it) => {
            const selected = selectedId === it.id;
            return (
              <button
                key={it.id}
                onClick={() => setSelectedId(it.id)}
                style={{
                  textAlign: "left",
                  padding: "10px 12px",
                  border: `1px solid ${selected ? tc.accent : tc.border}`,
                  background: selected ? tc.bgCard : "transparent",
                  borderRadius: 8,
                  cursor: "pointer",
                  overflow: "hidden",
                  minWidth: 0,
                  color: tc.text,
                }}
              >
                <div style={{ display: "flex", gap: 6, alignItems: "center", minWidth: 0 }}>
                  <span
                    style={{
                      fontSize: 9,
                      fontWeight: 700,
                      padding: "1px 6px",
                      borderRadius: 3,
                      background: KIND_COLORS[it.kind] + "22",
                      color: KIND_COLORS[it.kind],
                      flexShrink: 0,
                    }}
                  >
                    {KIND_LABELS[it.kind]}
                  </span>
                  <span
                    style={{
                      fontWeight: 600,
                      fontSize: 13,
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                      flex: 1,
                      minWidth: 0,
                    }}
                    title={it.title}
                  >
                    {it.title}
                  </span>
                </div>
                <div
                  style={{
                    fontSize: 10,
                    color: tc.textMuted,
                    marginTop: 2,
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                >
                  {it.vault_file_path}
                </div>
              </button>
            );
          })}
        </div>

        <div
          style={{
            flex: 1,
            minWidth: 0,
            padding: 16,
            background: tc.bgCard,
            border: `1px solid ${tc.border}`,
            borderRadius: 8,
            overflow: "auto",
            maxHeight: "calc(100vh - 360px)",
          }}
        >
          {!selected && (
            <div style={{ color: tc.textMuted, fontSize: 13, textAlign: "center", padding: 40 }}>
              Seleziona una nota dalla lista per visualizzarla.
            </div>
          )}
          {selected && (
            <div>
              <div style={{ display: "flex", gap: 6, alignItems: "center", marginBottom: 8, flexWrap: "wrap" }}>
                <span
                  style={{
                    fontSize: 10,
                    fontWeight: 700,
                    padding: "2px 8px",
                    borderRadius: 4,
                    background: KIND_COLORS[selected.kind] + "22",
                    color: KIND_COLORS[selected.kind],
                  }}
                >
                  {KIND_LABELS[selected.kind]}
                </span>
                <span style={{ fontSize: 10, color: tc.textMuted }}>
                  {selected.auto_generated ? "auto-generata" : "curata manualmente"}
                </span>
                <button
                  onClick={() => openInObsidian(selected.vault_file_path)}
                  title="Apri questa nota in Obsidian"
                  style={{
                    marginLeft: "auto",
                    padding: "3px 10px",
                    fontSize: 11,
                    background: "#7c3aed",
                    color: "#fff",
                    border: "none",
                    borderRadius: 6,
                    cursor: "pointer",
                  }}
                >
                  Apri in Obsidian
                </button>
              </div>
              <h2 style={{ fontSize: 18, fontWeight: 700, margin: "0 0 4px" }}>{selected.title}</h2>
              <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 12 }}>
                {selected.vault_file_path}
              </div>
              <div
                style={{ fontSize: 13, lineHeight: 1.6 }}
                className="meta-docs-md"
              >
                <MarkdownBlock content={selected.body_md} skipNormalize />
              </div>
              <style jsx>{`
                .meta-docs-md :global(code) {
                  background: transparent !important;
                  padding: 0 !important;
                  border: none !important;
                  font-family: "JetBrains Mono", "Fira Code", monospace !important;
                  font-size: 12px !important;
                  color: ${tc.textSecondary} !important;
                }
                .meta-docs-md :global(li code),
                .meta-docs-md :global(p code) {
                  background: ${tc.bgHover} !important;
                  padding: 1px 5px !important;
                  border-radius: 3px !important;
                }
                .meta-docs-md :global(table) {
                  border-collapse: collapse;
                  margin: 8px 0;
                  font-size: 12px;
                }
                .meta-docs-md :global(th),
                .meta-docs-md :global(td) {
                  border: 1px solid ${tc.border};
                  padding: 4px 8px;
                  text-align: left;
                }
                .meta-docs-md :global(h2) {
                  font-size: 15px;
                  margin: 16px 0 6px;
                }
                .meta-docs-md :global(h3) {
                  font-size: 13px;
                  margin: 12px 0 4px;
                }
              `}</style>
              {(selected.incoming_links?.length ?? 0) > 0 && (
                <div style={{ marginTop: 20, paddingTop: 12, borderTop: `1px solid ${tc.border}` }}>
                  <div style={{ fontSize: 12, fontWeight: 700, marginBottom: 6 }}>Backlinks</div>
                  {selected.incoming_links.map((l, i) => (
                    <div key={i} style={{ fontSize: 12, color: tc.textMuted, marginBottom: 2 }}>
                      <span style={{ color: tc.accent }}>{l.rel_type}</span>: {l.title}
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
      </div>

      <KnowledgeGraph mode="meta" open={graphOpen} onClose={() => setGraphOpen(false)} />
    </div>
  );
}
