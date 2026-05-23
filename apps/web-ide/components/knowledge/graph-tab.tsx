"use client";

import { useEffect, useState } from "react";
import { useI18n } from "../../lib/i18n";
import { KnowledgeGraph } from "./knowledge-graph";
import {
  getObsidianVaultName,
  putObsidianVaultName,
  recomputeKnowledgeLinks,
} from "../../lib/api-client";

interface Props {
  projectId: string;
  /**
   * Vault path relativo (es. `.nexus/knowledge`) usato solo come hint
   * nelle istruzioni di setup Obsidian.
   */
  vaultPathHint?: string;
}

/**
 * GraphTab: due azioni principali per esplorare le relazioni del vault:
 *  1. Mostra grafo in-app (Cytoscape dialog modale) — sempre disponibile
 *  2. Apri in Obsidian (deep link `obsidian://`) — richiede vault registrato
 *     sull'app desktop dell'utente. Il nome del vault e' configurabile qui.
 */
export function GraphTab({ projectId, vaultPathHint }: Props) {
  const { t } = useI18n();
  const [graphOpen, setGraphOpen] = useState(false);
  const [vaultName, setVaultName] = useState("");
  const [savedVaultName, setSavedVaultName] = useState("");
  const [savingVault, setSavingVault] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [recomputing, setRecomputing] = useState(false);
  const [recomputeMsg, setRecomputeMsg] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getObsidianVaultName(projectId)
      .then((r) => {
        if (cancelled) return;
        setVaultName(r.obsidian_vault_name ?? "");
        setSavedVaultName(r.obsidian_vault_name ?? "");
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [projectId]);

  const saveVaultName = async () => {
    setSavingVault(true);
    try {
      const r = await putObsidianVaultName(projectId, vaultName.trim());
      setSavedVaultName(r.obsidian_vault_name);
      setShowSettings(false);
    } finally {
      setSavingVault(false);
    }
  };

  const handleRecompute = async () => {
    setRecomputing(true);
    setRecomputeMsg(null);
    try {
      const r = await recomputeKnowledgeLinks(projectId);
      setRecomputeMsg(`${r.links_created} link su ${r.notes_processed} note`);
    } catch (e) {
      setRecomputeMsg("Errore: " + (e instanceof Error ? e.message : String(e)));
    } finally {
      setRecomputing(false);
    }
  };

  const openInObsidian = () => {
    if (!savedVaultName) {
      setShowSettings(true);
      return;
    }
    // Apre Obsidian sulla home del vault (Obsidian poi mostra la graph view manualmente
    // via Ctrl+G se l'utente lo desidera).
    const url = `obsidian://open?vault=${encodeURIComponent(savedVaultName)}`;
    window.location.href = url;
  };

  return (
    <div style={{ padding: 12, overflow: "hidden", minWidth: 0 }}>
      <p style={{ fontSize: 12, color: "#525252", margin: "0 0 12px" }}>
        {t("knowledge.graph.placeholder") || "Esplora le relazioni tra le note del vault."}
      </p>

      <button
        onClick={() => setGraphOpen(true)}
        style={{
          width: "100%",
          padding: "10px 12px",
          fontSize: 13,
          fontWeight: 600,
          background: "#171717",
          color: "#fff",
          border: "none",
          borderRadius: 8,
          cursor: "pointer",
          marginBottom: 8,
        }}
      >
        Apri grafo (Cytoscape)
      </button>

      <button
        onClick={openInObsidian}
        title={
          savedVaultName
            ? `Apre Obsidian sul vault "${savedVaultName}"`
            : "Configura il nome del vault Obsidian"
        }
        style={{
          width: "100%",
          padding: "10px 12px",
          fontSize: 13,
          fontWeight: 600,
          background: "#7c3aed",
          color: "#fff",
          border: "none",
          borderRadius: 8,
          cursor: "pointer",
          marginBottom: 8,
        }}
      >
        Apri in Obsidian
      </button>

      <button
        onClick={handleRecompute}
        disabled={recomputing}
        title="Ricalcola i link automatici tra le note del progetto"
        style={{
          width: "100%",
          padding: "8px 12px",
          fontSize: 12,
          fontWeight: 600,
          background: recomputing ? "#a3a3a3" : "#0ea5e9",
          color: "#fff",
          border: "none",
          borderRadius: 8,
          cursor: recomputing ? "default" : "pointer",
          marginBottom: 8,
        }}
      >
        {recomputing ? "Calcolo in corso..." : "Ricalcola link automatici"}
      </button>
      {recomputeMsg && (
        <div
          style={{
            fontSize: 11,
            color: recomputeMsg.startsWith("Errore") ? "#dc2626" : "#16a34a",
            marginBottom: 6,
          }}
        >
          {recomputeMsg}
        </div>
      )}

      <button
        onClick={() => setShowSettings((v) => !v)}
        style={{
          width: "100%",
          padding: "6px 10px",
          fontSize: 11,
          color: "#525252",
          background: "transparent",
          border: "1px solid #e5e5e5",
          borderRadius: 6,
          cursor: "pointer",
        }}
      >
        {showSettings ? "Nascondi configurazione" : "Configura vault Obsidian"}
      </button>

      {showSettings && (
        <div
          style={{
            marginTop: 8,
            padding: 10,
            background: "#fafafa",
            border: "1px solid #e5e5e5",
            borderRadius: 8,
            fontSize: 12,
            color: "#525252",
          }}
        >
          <p style={{ margin: "0 0 8px", fontSize: 11 }}>
            <strong>Setup Obsidian:</strong>
          </p>
          <ol style={{ margin: "0 0 8px", paddingLeft: 18, fontSize: 11, lineHeight: 1.5 }}>
            <li>Apri Obsidian</li>
            <li>File → Open vault → Open folder as vault</li>
            <li>
              Seleziona la cartella{" "}
              <code style={{ background: "#fff", padding: "1px 4px", borderRadius: 3 }}>
                {vaultPathHint || ".nexus/knowledge"}
              </code>{" "}
              dentro la root del progetto
            </li>
            <li>Obsidian ti chiedera' di dare un nome al vault. Inseriscilo qui sotto:</li>
          </ol>
          <input
            value={vaultName}
            onChange={(e) => setVaultName(e.target.value)}
            placeholder="Nome del vault (es. 'NomeProgetto')"
            style={{
              width: "100%",
              padding: "5px 8px",
              fontSize: 12,
              border: "1px solid #d4d4d4",
              borderRadius: 6,
              outline: "none",
              boxSizing: "border-box",
              marginBottom: 6,
            }}
          />
          <div style={{ display: "flex", gap: 4, justifyContent: "flex-end" }}>
            <button
              onClick={() => {
                setVaultName(savedVaultName);
                setShowSettings(false);
              }}
              disabled={savingVault}
              style={{
                padding: "4px 10px",
                fontSize: 11,
                background: "transparent",
                border: "1px solid #d4d4d4",
                borderRadius: 6,
                cursor: savingVault ? "default" : "pointer",
              }}
            >
              Annulla
            </button>
            <button
              onClick={saveVaultName}
              disabled={savingVault || vaultName.trim() === savedVaultName}
              style={{
                padding: "4px 10px",
                fontSize: 11,
                fontWeight: 600,
                background: savingVault ? "#a3a3a3" : "#171717",
                color: "#fff",
                border: "none",
                borderRadius: 6,
                cursor: savingVault ? "default" : "pointer",
              }}
            >
              {savingVault ? "Salvataggio..." : "Salva"}
            </button>
          </div>
          {savedVaultName && (
            <p style={{ margin: "8px 0 0", fontSize: 10, color: "#a3a3a3" }}>
              Vault corrente:{" "}
              <code style={{ background: "#fff", padding: "1px 4px", borderRadius: 3 }}>
                {savedVaultName}
              </code>
            </p>
          )}
        </div>
      )}

      <KnowledgeGraph
        mode="project"
        projectId={projectId}
        open={graphOpen}
        onClose={() => setGraphOpen(false)}
      />
    </div>
  );
}
