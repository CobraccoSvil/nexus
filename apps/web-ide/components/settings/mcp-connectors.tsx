"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import {
  createMcpServer,
  deleteMcpServer,
  listMcpServers,
  testMcpServer,
  toggleMcpServer,
  type McpServer,
  type McpServerTool,
} from "../../lib/api-client";
import { useThemeColors } from "../../lib/theme";
import { useGlobalDialog } from "../global-dialog-provider";
import { McpRegistrySearch } from "./mcp-registry-search";
import { type CatalogEntry } from "./mcp-catalog-data";

// ── Helpers ────────────────────────────────────────────────────────────────

function transportIcon(transport: string) {
  return transport === "stdio" ? "⚙️" : "🌐";
}

function statusDot(enabled: boolean) {
  return (
    <span
      style={{
        display: "inline-block",
        width: 8,
        height: 8,
        borderRadius: "50%",
        background: enabled ? "#22c55e" : "#6b7280",
        boxShadow: enabled ? "0 0 0 2px #22c55e40" : undefined,
        flexShrink: 0,
      }}
    />
  );
}

// ── Modale di aggiunta ────────────────────────────────────────────────────

interface CatalogPrefill {
  transport: "http" | "stdio";
  name: string;
  description?: string;
  url?: string;
  command?: string;
  args?: string;
  envVars?: string;
}

interface AddServerModalProps {
  onClose: () => void;
  onCreated: (srv: McpServer) => void;
  tc: ReturnType<typeof useThemeColors>;
  prefill?: CatalogPrefill;
}

function AddServerModal({ onClose, onCreated, tc, prefill }: AddServerModalProps) {
  const [transport, setTransport] = useState<"http" | "stdio">(prefill?.transport ?? "stdio");
  const [name, setName] = useState(prefill?.name ?? "");
  const [description, setDescription] = useState(prefill?.description ?? "");
  const [url, setUrl] = useState(prefill?.url ?? "");
  const [command, setCommand] = useState(prefill?.command ?? "");
  const [args, setArgs] = useState(prefill?.args ?? ""); // spazio-separati
  const [headers, setHeaders] = useState(""); // KEY=VALUE per riga
  const [envVars, setEnvVars] = useState(prefill?.envVars ?? "");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const parseKv = (raw: string): Record<string, string> => {
    const result: Record<string, string> = {};
    for (const line of raw.split("\n")) {
      const eq = line.indexOf("=");
      if (eq > 0) {
        result[line.slice(0, eq).trim()] = line.slice(eq + 1).trim();
      }
    }
    return result;
  };

  const handleSubmit = async () => {
    if (!name.trim()) { setError("Nome richiesto"); return; }
    if (transport === "http" && !url.trim()) { setError("URL richiesto"); return; }
    if (transport === "stdio" && !command.trim()) { setError("Comando richiesto"); return; }
    setSaving(true);
    setError(null);
    try {
      const srv = await createMcpServer({
        name: name.trim(),
        description: description.trim() || undefined,
        transport,
        url: transport === "http" ? url.trim() : undefined,
        command: transport === "stdio" ? command.trim() : undefined,
        args: transport === "stdio" ? args.trim().split(/\s+/).filter(Boolean) : [],
        headers: transport === "http" ? parseKv(headers) : {},
        envVars: parseKv(envVars),
      });
      onCreated(srv);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore creazione server");
    } finally {
      setSaving(false);
    }
  };

  const inputStyle: React.CSSProperties = {
    width: "100%",
    padding: "6px 10px",
    borderRadius: 6,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.text,
    fontSize: 13,
    outline: "none",
    boxSizing: "border-box",
  };

  const labelStyle: React.CSSProperties = {
    display: "block",
    fontSize: 11,
    color: tc.textMuted,
    marginBottom: 4,
    fontWeight: 600,
    textTransform: "uppercase",
    letterSpacing: "0.05em",
  };

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.55)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 1000,
      }}
      onClick={onClose}
    >
      <div
        style={{
          background: tc.bgCard,
          border: `1px solid ${tc.border}`,
          borderRadius: 12,
          padding: 24,
          width: 480,
          maxWidth: "95vw",
          maxHeight: "90vh",
          overflowY: "auto",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <div style={{ fontWeight: 700, fontSize: 16, marginBottom: 20, color: tc.text }}>
          Aggiungi server MCP
        </div>

        {prefill && (
          <div style={{
            padding: "10px 14px",
            borderRadius: 8,
            background: `${tc.accent}12`,
            border: `1px solid ${tc.accent}40`,
            color: tc.accent,
            fontSize: 12,
            marginBottom: 16,
          }}>
            💡 Configurazione pre-compilata dal catalogo. Compila le variabili d&apos;ambiente prima di salvare.
          </div>
        )}

        {/* Transport toggle */}
        <div style={{ marginBottom: 16 }}>
          <label style={labelStyle}>Tipo connessione</label>
          <div style={{ display: "flex", gap: 8 }}>
            {(["http", "stdio"] as const).map((t) => (
              <button
                key={t}
                onClick={() => setTransport(t)}
                style={{
                  flex: 1,
                  padding: "8px 0",
                  borderRadius: 8,
                  border: `2px solid ${transport === t ? tc.accent : tc.border}`,
                  background: transport === t ? `${tc.accent}18` : tc.bgInput,
                  color: transport === t ? tc.accent : tc.textMuted,
                  fontWeight: transport === t ? 700 : 400,
                  fontSize: 13,
                  cursor: "pointer",
                }}
              >
                {t === "http" ? "🌐 HTTP" : "⚙️ Stdio (locale)"}
              </button>
            ))}
          </div>
        </div>

        {/* Nome */}
        <div style={{ marginBottom: 14 }}>
          <label style={labelStyle}>Nome *</label>
          <input
            style={inputStyle}
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="Es. GitHub, Stripe, Database locale"
          />
        </div>

        {/* Descrizione */}
        <div style={{ marginBottom: 14 }}>
          <label style={labelStyle}>Descrizione</label>
          <input
            style={inputStyle}
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder="Facoltativo"
          />
        </div>

        {transport === "http" ? (
          <>
            <div style={{ marginBottom: 14 }}>
              <label style={labelStyle}>URL endpoint MCP *</label>
              <input
                style={inputStyle}
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                placeholder="https://mcp.example.com/mcp"
              />
            </div>
            <div style={{ marginBottom: 14 }}>
              <label style={labelStyle}>Header HTTP (KEY=VALUE per riga)</label>
              <textarea
                style={{ ...inputStyle, height: 72, resize: "vertical", fontFamily: "monospace" }}
                value={headers}
                onChange={(e) => setHeaders(e.target.value)}
                placeholder={"Authorization=Bearer sk-...\nX-Custom=value"}
              />
            </div>
          </>
        ) : (
          <>
            <div style={{ marginBottom: 14 }}>
              <label style={labelStyle}>Comando *</label>
              <input
                style={inputStyle}
                value={command}
                onChange={(e) => setCommand(e.target.value)}
                placeholder="npx"
              />
            </div>
            <div style={{ marginBottom: 14 }}>
              <label style={labelStyle}>Argomenti (separati da spazio)</label>
              <input
                style={inputStyle}
                value={args}
                onChange={(e) => setArgs(e.target.value)}
                placeholder="-y @modelcontextprotocol/server-filesystem /path/to/dir"
              />
            </div>
            <div style={{ marginBottom: 14 }}>
              <label style={labelStyle}>Variabili d&apos;ambiente (KEY=VALUE per riga)</label>
              <textarea
                style={{ ...inputStyle, height: 72, resize: "vertical", fontFamily: "monospace" }}
                value={envVars}
                onChange={(e) => setEnvVars(e.target.value)}
                placeholder={"GITHUB_TOKEN=ghp_...\nAPI_KEY=..."}
              />
            </div>
          </>
        )}

        {error && (
          <div
            style={{
              padding: "6px 10px",
              borderRadius: 6,
              background: `${tc.error}18`,
              border: `1px solid ${tc.error}`,
              color: tc.error,
              fontSize: 12,
              marginBottom: 16,
            }}
          >
            {error}
          </div>
        )}

        <div style={{ display: "flex", gap: 10, justifyContent: "flex-end" }}>
          <button
            onClick={onClose}
            style={{
              padding: "8px 18px",
              borderRadius: 8,
              border: `1px solid ${tc.border}`,
              background: "transparent",
              color: tc.textMuted,
              fontSize: 13,
              cursor: "pointer",
            }}
          >
            Annulla
          </button>
          <button
            onClick={() => void handleSubmit()}
            disabled={saving}
            style={{
              padding: "8px 18px",
              borderRadius: 8,
              border: "none",
              background: tc.accent,
              color: "#fff",
              fontWeight: 700,
              fontSize: 13,
              cursor: saving ? "not-allowed" : "pointer",
              opacity: saving ? 0.7 : 1,
            }}
          >
            {saving ? "Salvataggio…" : "Aggiungi"}
          </button>
        </div>
      </div>
    </div>
  );
}

// ── Tool list collassabile ────────────────────────────────────────────────

function ToolList({ tools }: { tools: McpServerTool[] }) {
  const tc = useThemeColors();
  const [expanded, setExpanded] = useState(false);
  if (tools.length === 0) return null;

  return (
    <div style={{ marginTop: 8 }}>
      <button
        onClick={() => setExpanded((p) => !p)}
        style={{
          background: "none",
          border: "none",
          color: tc.accent,
          fontSize: 11,
          cursor: "pointer",
          padding: 0,
          fontWeight: 600,
        }}
      >
        {expanded ? "▾" : "▸"} {tools.length} tool disponibili
      </button>
      {expanded && (
        <div
          style={{
            marginTop: 6,
            display: "flex",
            flexWrap: "wrap",
            gap: 4,
          }}
        >
          {tools.map((t) => (
            <span
              key={t.name}
              title={t.description}
              style={{
                padding: "2px 8px",
                borderRadius: 999,
                background: `${tc.accent}18`,
                border: `1px solid ${tc.accent}40`,
                color: tc.accent,
                fontSize: 11,
                fontFamily: "monospace",
              }}
            >
              {t.name}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

// ── Card singolo server ───────────────────────────────────────────────────

interface ServerCardProps {
  server: McpServer;
  onToggle: (id: string, enabled: boolean) => Promise<void>;
  onDelete: (id: string) => Promise<void>;
  onConfirmDelete: (serverName: string) => Promise<boolean>;
  onRefresh: (id: string, tools: McpServerTool[]) => void;
  tc: ReturnType<typeof useThemeColors>;
}

function ServerCard({ server, onToggle, onDelete, onConfirmDelete, onRefresh, tc }: ServerCardProps) {
  const [testing, setTesting] = useState(false);
  const [toggling, setToggling] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const isManageable = server.canManage !== false;
  const [testResult, setTestResult] = useState<{
    success: boolean;
    toolCount: number;
    error?: string;
  } | null>(null);

  const handleTest = async () => {
    setTesting(true);
    setTestResult(null);
    try {
      const res = await testMcpServer(server.id);
      setTestResult({ success: res.success, toolCount: res.toolCount, error: res.error });
      if (res.success) {
        onRefresh(server.id, res.tools);
      }
    } catch (e) {
      setTestResult({ success: false, toolCount: 0, error: e instanceof Error ? e.message : "Errore" });
    } finally {
      setTesting(false);
    }
  };

  const handleToggleClick = async () => {
    if (toggling || !isManageable) return;
    setToggling(true);
    setTestResult(null);
    try {
      await onToggle(server.id, !server.enabled);
    } finally {
      setToggling(false);
    }
  };

  const handleDeleteClick = async () => {
    if (deleting || !isManageable) return;
    const confirmed = await onConfirmDelete(server.name);
    if (!confirmed) return;
    setDeleting(true);
    setTestResult(null);
    try {
      await onDelete(server.id);
    } finally {
      setDeleting(false);
    }
  };

  return (
    <div
      style={{
        padding: "14px 16px",
        borderRadius: 10,
        border: `1px solid ${tc.border}`,
        background: tc.bgCard,
        opacity: server.enabled ? 1 : 0.6,
        transition: "opacity 0.2s",
      }}
    >
      <div style={{ display: "flex", alignItems: "flex-start", gap: 10 }}>
        {/* Icona transport */}
        <span style={{ fontSize: 20, lineHeight: 1, marginTop: 1, flexShrink: 0 }}>
          {transportIcon(server.transport)}
        </span>

        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 2 }}>
            {statusDot(server.enabled)}
            <span style={{ fontWeight: 700, fontSize: 14, color: tc.text }}>
              {server.name}
            </span>
            <span
              style={{
                fontSize: 10,
                padding: "1px 6px",
                borderRadius: 999,
                background: tc.bgInput,
                color: tc.textMuted,
                border: `1px solid ${tc.border}`,
                textTransform: "uppercase",
                fontWeight: 600,
              }}
            >
              {server.transport}
            </span>
            <span
              style={{
                fontSize: 10,
                padding: "1px 6px",
                borderRadius: 999,
                background: server.scope === "global" ? `${tc.accent}16` : tc.bgInput,
                color: server.scope === "global" ? tc.accent : tc.textMuted,
                border: `1px solid ${server.scope === "global" ? `${tc.accent}33` : tc.border}`,
                textTransform: "uppercase",
                fontWeight: 600,
              }}
            >
              {server.scope}
            </span>
          </div>

          {server.description && (
            <div style={{ fontSize: 12, color: tc.textMuted, marginBottom: 4 }}>
              {server.description}
            </div>
          )}

          <div
            style={{
              fontSize: 11,
              fontFamily: "monospace",
              color: tc.textMuted,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {server.transport === "http"
              ? server.url
              : `${server.command} ${server.args.join(" ")}`}
          </div>

          {/* Tool list */}
          {server.tools && server.tools.length > 0 && (
            <ToolList tools={server.tools} />
          )}

          {/* Test result */}
          {testResult && (
            <div
              style={{
                marginTop: 8,
                fontSize: 12,
                padding: "4px 10px",
                borderRadius: 6,
                background: testResult.success ? "#22c55e18" : `${tc.error}18`,
                border: `1px solid ${testResult.success ? "#22c55e40" : `${tc.error}40`}`,
                color: testResult.success ? "#16a34a" : tc.error,
              }}
            >
              {testResult.success
                ? `✓ Connesso — ${testResult.toolCount} tool scoperti`
                : `✗ Errore: ${testResult.error}`}
            </div>
          )}
        </div>

        {/* Azioni */}
        <div style={{ display: "flex", flexDirection: "column", gap: 6, flexShrink: 0 }}>
          {/* Toggle */}
          <button
            onClick={() => void handleToggleClick()}
            disabled={toggling || !isManageable}
            title={
              !isManageable
                ? "Gestito da admin"
                : server.enabled
                  ? "Disabilita"
                  : "Abilita"
            }
            style={{
              width: 40,
              height: 22,
              borderRadius: 999,
              border: "none",
              cursor: !isManageable ? "not-allowed" : toggling ? "wait" : "pointer",
              position: "relative",
              background: server.enabled ? tc.accent : tc.border,
              transition: "background 0.2s",
              opacity: !isManageable || toggling ? 0.7 : 1,
            }}
          >
            <span
              style={{
                position: "absolute",
                top: 3,
                left: server.enabled ? 21 : 3,
                width: 16,
                height: 16,
                borderRadius: "50%",
                background: "#fff",
                transition: "left 0.2s",
              }}
            />
          </button>

          {/* Test */}
          <button
            onClick={() => void handleTest()}
            disabled={testing}
            title="Testa connessione"
            style={{
              padding: "4px 8px",
              borderRadius: 6,
              border: `1px solid ${tc.border}`,
              background: "transparent",
              color: tc.textMuted,
              fontSize: 11,
              cursor: testing ? "wait" : "pointer",
              whiteSpace: "nowrap",
            }}
          >
            {testing ? "…" : "Test"}
          </button>

          {/* Elimina */}
          <button
            onClick={() => void handleDeleteClick()}
            disabled={!isManageable || deleting}
            title={isManageable ? "Elimina" : "Gestito da admin"}
            style={{
              padding: "4px 8px",
              borderRadius: 6,
              border: `1px solid ${tc.error}40`,
              background: "transparent",
              color: tc.error,
              fontSize: 11,
              cursor: !isManageable ? "not-allowed" : deleting ? "wait" : "pointer",
              opacity: !isManageable || deleting ? 0.5 : 1,
              whiteSpace: "nowrap",
            }}
          >
            {deleting ? "…" : "Elimina"}
          </button>
        </div>
      </div>
    </div>
  );
}

// ── Pannello principale ───────────────────────────────────────────────────

export function McpConnectors() {
  const tc = useThemeColors();
  const { confirmDialog } = useGlobalDialog();
  const [servers, setServers] = useState<McpServer[]>([]);
  const [loading, setLoading] = useState(true);
  const [showAdd, setShowAdd] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const loadedRef = useRef(false);
  const [activeTab, setActiveTab] = useState<"list" | "catalog">("list");
  const [prefill, setPrefill] = useState<CatalogPrefill | null>(null);

  const loadServers = useCallback(async () => {
    try {
      const res = await listMcpServers();
      setServers(res.servers);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore caricamento server");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (loadedRef.current) return;
    loadedRef.current = true;
    void loadServers();
  }, [loadServers]);

  const handleToggle = useCallback(async (id: string, enabled: boolean) => {
    setError(null);
    try {
      await toggleMcpServer(id, enabled);
      setServers((prev) => prev.map((s) => (s.id === id ? { ...s, enabled } : s)));
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore aggiornamento connettore");
    }
  }, []);

  const handleDelete = useCallback(async (id: string) => {
    setError(null);
    try {
      await deleteMcpServer(id);
      setServers((prev) => prev.filter((s) => s.id !== id));
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore eliminazione connettore");
    }
  }, []);

  const handleRefreshTools = useCallback((id: string, tools: McpServerTool[]) => {
    setServers((prev) =>
      prev.map((s) => (s.id === id ? { ...s, tools } : s)),
    );
  }, []);

  const handleConfirmDelete = useCallback(
    async (serverName: string) =>
      confirmDialog(
        `Vuoi eliminare il connettore MCP "${serverName}"?`,
        "Conferma eliminazione connettore MCP",
      ),
    [confirmDialog],
  );

  const handleCreated = useCallback((srv: McpServer) => {
    setServers((prev) => [srv, ...prev]);
  }, []);

  const handleAddFromCatalog = (entry: CatalogEntry) => {
    setPrefill({
      transport: entry.transport,
      name: entry.name,
      description: entry.description,
      url: entry.url,
      command: entry.command,
      args: entry.args?.join(" "),
      envVars: entry.requiredEnvVars?.map((k) => `${k}=`).join("\n") ?? "",
    });
    setShowAdd(true);
  };

  const sectionTitle: React.CSSProperties = {
    fontSize: 13,
    fontWeight: 700,
    color: tc.text,
    marginBottom: 4,
  };

  const sectionDesc: React.CSSProperties = {
    fontSize: 12,
    color: tc.textMuted,
    marginBottom: 16,
    lineHeight: 1.5,
  };

  return (
    <div>
      {/* Header */}
      <div style={{ display: "flex", alignItems: "flex-start", justifyContent: "space-between", marginBottom: 20 }}>
        <div>
          <div style={sectionTitle}>Connettori MCP</div>
          <div style={sectionDesc}>
            Collega server MCP esterni per estendere le capacità degli agenti AI con tool aggiuntivi
            (database, API, servizi cloud, filesystem locale, ecc.).
          </div>
        </div>
        <button
          onClick={() => setShowAdd(true)}
          style={{
            display: "flex",
            alignItems: "center",
            gap: 6,
            padding: "8px 14px",
            borderRadius: 8,
            border: `1px solid ${tc.accent}`,
            background: `${tc.accent}18`,
            color: tc.accent,
            fontWeight: 700,
            fontSize: 13,
            cursor: "pointer",
            flexShrink: 0,
          }}
        >
          <span>+</span>
          <span>Aggiungi</span>
        </button>
      </div>

      {/* Tab bar */}
      <div style={{ display: "flex", borderBottom: `1px solid ${tc.border}`, marginBottom: 20 }}>
        {(["list", "catalog"] as const).map((tab) => (
          <button
            key={tab}
            onClick={() => setActiveTab(tab)}
            style={{
              padding: "8px 18px",
              fontSize: 13,
              fontWeight: activeTab === tab ? 600 : 400,
              color: activeTab === tab ? tc.accent : tc.textMuted,
              background: "transparent",
              border: "none",
              borderBottom: `2px solid ${activeTab === tab ? tc.accent : "transparent"}`,
              cursor: "pointer",
              marginBottom: -1,
            }}
          >
            {tab === "list" ? "I miei connettori" : "📦 Catalogo"}
          </button>
        ))}
      </div>

      {activeTab === "list" && (<>
      {/* Stato */}
      {loading && (
        <div style={{ color: tc.textMuted, fontSize: 12 }}>Caricamento connettori…</div>
      )}

      {error && (
        <div
          style={{
            padding: "8px 12px",
            borderRadius: 8,
            background: `${tc.error}18`,
            border: `1px solid ${tc.error}`,
            color: tc.error,
            fontSize: 12,
            marginBottom: 12,
          }}
        >
          {error}
        </div>
      )}

      {/* Lista server */}
      {!loading && servers.length === 0 && (
        <div
          style={{
            padding: "32px 20px",
            textAlign: "center",
            border: `2px dashed ${tc.border}`,
            borderRadius: 12,
            color: tc.textMuted,
            fontSize: 13,
          }}
        >
          <div style={{ fontSize: 32, marginBottom: 12 }}>🔌</div>
          <div style={{ fontWeight: 600, marginBottom: 6 }}>Nessun connettore configurato</div>
          <div style={{ fontSize: 12, marginBottom: 16 }}>
            Aggiungi server MCP per dare agli agenti accesso a tool esterni
          </div>
          <button
            onClick={() => setShowAdd(true)}
            style={{
              padding: "8px 20px",
              borderRadius: 8,
              border: "none",
              background: tc.accent,
              color: "#fff",
              fontWeight: 700,
              fontSize: 13,
              cursor: "pointer",
            }}
          >
            Aggiungi il primo connettore
          </button>
        </div>
      )}

      <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
        {servers.map((srv) => (
          <ServerCard
            key={srv.id}
            server={srv}
            onToggle={handleToggle}
            onDelete={handleDelete}
            onConfirmDelete={handleConfirmDelete}
            onRefresh={handleRefreshTools}
            tc={tc}
          />
        ))}
      </div>
      </>)}

      {activeTab === "catalog" && (
        <McpRegistrySearch tc={tc} onAddEntry={handleAddFromCatalog} existingServers={servers} />
      )}

      {/* Modale aggiunta */}
      {showAdd && (
        <AddServerModal tc={tc} onClose={() => { setShowAdd(false); setPrefill(null); }} onCreated={handleCreated} prefill={prefill ?? undefined} />
      )}
    </div>
  );
}
