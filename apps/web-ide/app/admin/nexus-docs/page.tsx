"use client";

/**
 * Admin / Doc Nexus
 *
 * Wiki Confluence-like del meta-progetto Nexus: architettura, ADR, runbook,
 * schema DB, changelog auto-generato, decisioni estratte da chat. Tutto vive
 * sotto `docs/.nexus-vault/` (filesystem) sincronizzato con `nexus_meta_docs`
 * (DB). La UI usa il componente condiviso `WikiShell` parametrizzato sullo
 * scope "meta" — la stessa shell serve anche la KB dei progetti.
 */

import { useEffect, useState } from "react";
import {
  triggerMetaDocsRefresh,
  recomputeMetaDocsLinks,
} from "../../../lib/api-client";
import { useThemeColors } from "../../../lib/theme";
import { WikiShell } from "../../../components/wiki/wiki-shell";
import { makeMetaScope } from "../../../components/wiki/wiki-scope";
import { KnowledgeGraph } from "../../../components/knowledge/knowledge-graph";
import { AdminPageHeader } from "../../../components/admin/AdminPageHeader";

const META_VAULT_NAME_KEY = "nexus.meta_docs.obsidian_vault_name";

export default function NexusDocsAdminPage() {
  const tc = useThemeColors();
  const [refreshing, setRefreshing] = useState(false);
  const [recomputingLinks, setRecomputingLinks] = useState(false);
  const [graphOpen, setGraphOpen] = useState(false);
  const [vaultName, setVaultName] = useState("");
  const [showVaultSettings, setShowVaultSettings] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [scope] = useState(() => makeMetaScope());

  useEffect(() => {
    try {
      setVaultName(localStorage.getItem(META_VAULT_NAME_KEY) ?? "");
    } catch {
      // ignore
    }
  }, []);

  const handleRefresh = async () => {
    setRefreshing(true);
    setMsg(null);
    try {
      const r = await triggerMetaDocsRefresh();
      setMsg(
        `Refresh completato. Generate: ${r.generated ?? 0}, applicate: ${r.applied ?? 0}, saltate: ${r.skipped ?? 0}`,
      );
    } catch (e) {
      setMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setRefreshing(false);
    }
  };

  const handleRecomputeLinks = async () => {
    setRecomputingLinks(true);
    setMsg(null);
    try {
      const r = await recomputeMetaDocsLinks();
      const sem = r.semantic_links_created ?? 0;
      setMsg(
        `Wikilink: ${r.wikilinks_created} su ${r.notes_processed} note (${r.wikilinks_unresolved} non risolti). Link semantici: ${sem}.`,
      );
    } catch (e) {
      setMsg("Errore: " + (e instanceof Error ? e.message : String(e)));
    } finally {
      setRecomputingLinks(false);
    }
  };

  const saveVaultName = (name: string) => {
    setVaultName(name);
    try {
      if (name) localStorage.setItem(META_VAULT_NAME_KEY, name);
      else localStorage.removeItem(META_VAULT_NAME_KEY);
    } catch {
      // ignore
    }
  };

  const openInObsidian = () => {
    const name = vaultName.trim();
    if (!name) {
      setShowVaultSettings(true);
      return;
    }
    window.location.href = `obsidian://open?vault=${encodeURIComponent(name)}`;
  };

  const toolbar = (
    <div
      style={{
        display: "flex",
        gap: 6,
        alignItems: "center",
        flexWrap: "wrap",
        fontSize: 12,
      }}
    >
      <ToolbarAction
        onClick={handleRefresh}
        disabled={refreshing}
        bg={tc.accent}
        title="Rigenera tutta la doc dai generatori automatici"
      >
        {refreshing ? "Refresh in corso..." : "Rigenera doc"}
      </ToolbarAction>
      <ToolbarAction
        onClick={() => setGraphOpen(true)}
        bg="#171717"
        title="Mostra il grafo dei link tra documenti"
      >
        Grafo
      </ToolbarAction>
      <ToolbarAction
        onClick={handleRecomputeLinks}
        disabled={recomputingLinks}
        bg="#0ea5e9"
        title="Parsa i wikilink [[...]] e ricalcola le relazioni"
      >
        {recomputingLinks ? "Calcolo..." : "Ricalcola link"}
      </ToolbarAction>
      <ToolbarAction onClick={openInObsidian} bg="#7c3aed" title="Apri il vault in Obsidian">
        Apri in Obsidian
      </ToolbarAction>
      <a
        href="/api/meta-docs/export-archive"
        download
        style={{
          padding: "4px 10px",
          fontSize: 12,
          background: "#16a34a",
          color: "#fff",
          border: "none",
          borderRadius: 4,
          textDecoration: "none",
          display: "inline-block",
        }}
        title="Scarica un archivio del meta-vault (.tar.gz)"
      >
        Scarica vault
      </a>
      <button
        type="button"
        onClick={() => setShowVaultSettings((v) => !v)}
        style={{
          padding: "4px 10px",
          fontSize: 11,
          background: "transparent",
          color: tc.textMuted,
          border: `1px solid ${tc.border}`,
          borderRadius: 4,
          cursor: "pointer",
        }}
      >
        {showVaultSettings ? "Nascondi setup" : "Setup Obsidian"}
      </button>
      {msg && (
        <span
          style={{
            padding: "2px 8px",
            color: tc.textSecondary,
            fontSize: 11,
            flexBasis: "100%",
          }}
        >
          {msg}{" "}
          <button
            onClick={() => setMsg(null)}
            style={{
              background: "none",
              border: "none",
              color: tc.accent,
              cursor: "pointer",
              fontSize: 11,
            }}
          >
            chiudi
          </button>
        </span>
      )}
      {showVaultSettings && (
        <div
          style={{
            flexBasis: "100%",
            padding: 10,
            marginTop: 6,
            background: tc.bgCard,
            border: `1px solid ${tc.border}`,
            borderRadius: 6,
            fontSize: 12,
          }}
        >
          <div style={{ marginBottom: 6, fontWeight: 600 }}>
            Configura il vault Obsidian (locale)
          </div>
          <ol style={{ margin: "0 0 6px", paddingLeft: 18, lineHeight: 1.6 }}>
            <li>Apri Obsidian</li>
            <li>
              File &rarr; Open folder as vault &rarr;{" "}
              <code style={{ background: tc.bgInput, padding: "1px 4px", borderRadius: 3 }}>
                /home/administrator/ideai/docs/.nexus-vault
              </code>
            </li>
            <li>Inserisci il nome del vault qui sotto:</li>
          </ol>
          <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
            <input
              value={vaultName}
              onChange={(e) => setVaultName(e.target.value)}
              placeholder='Es. "Nexus-MetaVault"'
              style={{
                flex: 1,
                padding: "4px 8px",
                fontSize: 12,
                border: `1px solid ${tc.border}`,
                borderRadius: 4,
                background: tc.bgInput,
                color: tc.text,
              }}
            />
            <button
              type="button"
              onClick={() => {
                saveVaultName(vaultName.trim());
                setShowVaultSettings(false);
              }}
              style={{
                padding: "4px 12px",
                fontSize: 12,
                fontWeight: 600,
                background: tc.accent,
                color: "#fff",
                border: "none",
                borderRadius: 4,
                cursor: "pointer",
              }}
            >
              Salva
            </button>
          </div>
        </div>
      )}
    </div>
  );

  return (
    <div style={{ padding: 16, width: "100%" }}>
      <div
        style={{
          padding: "10px 14px",
          marginBottom: 12,
          borderRadius: 6,
          background: "#f59e0b22",
          border: `1px solid #f59e0b`,
          color: tc.text,
          fontSize: 13,
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 12,
          flexWrap: "wrap",
        }}
      >
        <span>
          Pagina deprecata (ADR 0017 v2). Usa la nuova{" "}
          <strong>Knowledge Base</strong> unificata.
        </span>
        <a
          href="/admin/kb"
          style={{
            padding: "5px 12px",
            background: tc.accent,
            color: "#fff",
            borderRadius: 4,
            textDecoration: "none",
            fontSize: 12,
            fontWeight: 600,
          }}
        >
          Apri /admin/kb →
        </a>
      </div>
      <header style={{ marginBottom: 12 }}>
        <AdminPageHeader
        title="Documentazione Nexus"
        description="Wiki del meta-progetto: architettura, ADR, runbook, schema, changelog auto,
          decisioni. Editing live, cronologia revisioni e protezione dalla rigenerazione."
      />
      </header>
      <WikiShell scope={scope} title="Doc Nexus" toolbar={toolbar} />
      {graphOpen && (
        <KnowledgeGraph
          mode="meta"
          open={graphOpen}
          onClose={() => setGraphOpen(false)}
        />
      )}
    </div>
  );
}

function ToolbarAction({
  children,
  onClick,
  disabled,
  bg,
  title,
}: {
  children: React.ReactNode;
  onClick: () => void;
  disabled?: boolean;
  bg: string;
  title?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={title}
      style={{
        padding: "4px 10px",
        fontSize: 12,
        fontWeight: 600,
        background: disabled ? "#444" : bg,
        color: "#fff",
        border: "none",
        borderRadius: 4,
        cursor: disabled ? "default" : "pointer",
        opacity: disabled ? 0.6 : 1,
      }}
    >
      {children}
    </button>
  );
}
