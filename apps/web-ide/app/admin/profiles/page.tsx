"use client";

import { useEffect, useState } from "react";
import { useThemeColors } from "../../../lib/theme";
import { useGlobalDialog } from "../../../components/global-dialog-provider";
import { AdminPageHeader } from "../../../components/admin/AdminPageHeader";
import {
  adminListProfiles,
  adminCreateProfile,
  adminUpdateProfile,
  adminDeleteProfile,
  adminListUserProfiles,
  adminListGlobalMcpServers,
  adminGetProfileMcpServers,
  adminSetProfileMcpServers,
  type UserProfile,
  type CreateProfilePayload,
  type GlobalMcpServer,
} from "../../../lib/api-client";

const EMOJI_OPTIONS = ["🤖","💙","⚛️","🐳","🐍","🦀","💚","📊","📱","☁️","🔐","🧪","📝","⚙️","🎯","🧠","🔧","🌐"];
const AUTOMATION_OPTIONS: { value: string; label: string }[] = [
  { value: "study",     label: "Studio — analisi silenziosa prima di agire" },
  { value: "confirm",   label: "Conferma — chiede approvazione prima di ogni azione" },
  { value: "automatic", label: "Automatico — esegue senza interruzioni" },
];

// ── Stili condivisi ──────────────────────────────────────────────────────────

function useStyles(tc: ReturnType<typeof useThemeColors>) {
  const input: React.CSSProperties = {
    background: tc.bgInput ?? tc.bg,
    border: `1px solid ${tc.border}`,
    borderRadius: 6,
    color: tc.text,
    fontSize: 13,
    padding: "5px 8px",
    fontFamily: "inherit",
    width: "100%",
    boxSizing: "border-box",
  };

  const btn = (variant: "primary" | "danger" | "ghost"): React.CSSProperties => ({
    border: `1px solid ${
      variant === "primary" ? tc.accent : variant === "danger" ? tc.error : tc.border
    }`,
    background:
      variant === "primary"
        ? `${tc.accent}22`
        : variant === "danger"
        ? `${tc.error}18`
        : "transparent",
    color:
      variant === "primary" ? tc.accent : variant === "danger" ? tc.error : tc.textMuted,
    borderRadius: 6,
    padding: "4px 10px",
    fontSize: 12,
    fontWeight: 600,
    cursor: "pointer",
    fontFamily: "inherit",
  });

  return { input, btn };
}

// ── Sezione MCP ──────────────────────────────────────────────────────────────

function McpSection({
  profileId,
  tc,
}: {
  profileId: string;
  tc: ReturnType<typeof useThemeColors>;
}) {
  const { btn } = useStyles(tc);
  const [allServers, setAllServers] = useState<GlobalMcpServer[]>([]);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    Promise.all([adminListGlobalMcpServers(), adminGetProfileMcpServers(profileId)])
      .then(([all, assigned]) => {
        if (cancelled) return;
        setAllServers(all.servers);
        setSelectedIds(new Set(assigned.servers.map((s) => s.id)));
      })
      .catch(() => {})
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [profileId]);

  const toggle = (id: string) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) { next.delete(id); } else { next.add(id); }
      return next;
    });
    setDirty(true);
  };

  const save = async () => {
    setSaving(true);
    try {
      await adminSetProfileMcpServers(profileId, Array.from(selectedIds));
      setDirty(false);
    } finally {
      setSaving(false);
    }
  };

  if (loading) {
    return <div style={{ color: tc.textMuted, fontSize: 12 }}>Caricamento server MCP…</div>;
  }

  if (allServers.length === 0) {
    return (
      <div style={{ color: tc.textMuted, fontSize: 12, fontStyle: "italic" }}>
        Nessun server MCP globale configurato. Aggiungine uno nella sezione Plugin MCP.
      </div>
    );
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
      {allServers.map((server) => (
        <label
          key={server.id}
          style={{
            display: "flex",
            alignItems: "center",
            gap: 10,
            padding: "7px 10px",
            borderRadius: 6,
            border: `1px solid ${selectedIds.has(server.id) ? tc.accent : tc.border}`,
            background: selectedIds.has(server.id) ? `${tc.accent}0d` : "transparent",
            cursor: "pointer",
            fontSize: 13,
          }}
        >
          <input
            type="checkbox"
            checked={selectedIds.has(server.id)}
            onChange={() => toggle(server.id)}
            style={{ accentColor: tc.accent }}
          />
          <div style={{ flex: 1 }}>
            <span style={{ fontWeight: 600, color: tc.text }}>{server.name}</span>
            {server.description && (
              <span style={{ color: tc.textMuted, marginLeft: 8 }}>{server.description}</span>
            )}
          </div>
          <span
            style={{
              fontSize: 10,
              color: tc.textMuted,
              background: `${tc.border}55`,
              borderRadius: 4,
              padding: "1px 6px",
            }}
          >
            {server.transport}
          </span>
        </label>
      ))}
      {dirty && (
        <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 4 }}>
          <button style={btn("primary")} onClick={() => void save()} disabled={saving}>
            {saving ? "Salvo…" : "Salva selezione MCP"}
          </button>
        </div>
      )}
    </div>
  );
}

// ── ProfileCard (profilo di sistema) ─────────────────────────────────────────

function ProfileCard({
  profile,
  onSave,
  onDelete,
}: {
  profile: UserProfile;
  onSave: (id: string, data: Partial<CreateProfilePayload>) => Promise<void>;
  onDelete: (id: string) => Promise<void>;
}) {
  const tc = useThemeColors();
  const { input, btn } = useStyles(tc);
  const { confirmDialog } = useGlobalDialog();
  const [editing, setEditing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [openSection, setOpenSection] = useState<"prompt" | "ai" | "mcp" | null>(null);

  const [name, setName] = useState(profile.name);
  const [emoji, setEmoji] = useState(profile.avatarEmoji);
  const [description, setDescription] = useState(profile.description ?? "");
  const [systemPrompt, setSystemPrompt] = useState(profile.systemPrompt);
  const [defaultProvider, setDefaultProvider] = useState(profile.defaultProvider ?? "");
  const [defaultModel, setDefaultModel] = useState(profile.defaultModel ?? "");
  const [defaultAutomation, setDefaultAutomation] = useState(profile.defaultAutomation ?? "");

  const handleSave = async () => {
    setSaving(true);
    try {
      await onSave(profile.id, {
        name,
        avatarEmoji: emoji,
        description,
        systemPrompt,
        defaultProvider: defaultProvider || undefined,
        defaultModel: defaultModel || undefined,
        defaultAutomation: defaultAutomation || undefined,
      });
      setEditing(false);
      setOpenSection(null);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    const ok = await confirmDialog({
      title: "Elimina profilo",
      message: `Eliminare il profilo "${profile.name}"?\n\nLe copie utente rimarranno.`,
      danger: true,
      confirmLabel: "Elimina",
      cancelLabel: "Annulla",
    });
    if (!ok) return;
    setDeleting(true);
    try { await onDelete(profile.id); } finally { setDeleting(false); }
  };

  const cardStyle: React.CSSProperties = {
    border: `1px solid ${editing ? tc.accent : tc.border}`,
    borderRadius: 10,
    background: tc.bgCard,
    overflow: "hidden",
  };

  const sectionHeader = (label: string, key: typeof openSection): React.CSSProperties => ({
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    padding: "8px 14px",
    background: openSection === key ? `${tc.accent}10` : `${tc.border}20`,
    borderTop: `1px solid ${tc.border}`,
    cursor: "pointer",
    fontSize: 12,
    fontWeight: 600,
    color: openSection === key ? tc.accent : tc.textMuted,
    userSelect: "none",
  });

  return (
    <div style={cardStyle}>
      {/* Header sempre visibile */}
      <div style={{ padding: "12px 14px", display: "flex", gap: 10, alignItems: "flex-start" }}>
        {editing ? (
          <select
            value={emoji}
            onChange={(e) => setEmoji(e.target.value)}
            style={{ ...input, width: 52, textAlign: "center", fontSize: 18, padding: "3px" }}
          >
            {EMOJI_OPTIONS.map((e) => <option key={e} value={e}>{e}</option>)}
          </select>
        ) : (
          <span style={{ fontSize: 22, lineHeight: 1, marginTop: 1 }}>{profile.avatarEmoji}</span>
        )}
        <div style={{ flex: 1, minWidth: 0 }}>
          {editing ? (
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Nome profilo"
              style={{ ...input, fontWeight: 700, fontSize: 14 }}
            />
          ) : (
            <div style={{ fontWeight: 700, fontSize: 14, color: tc.text }}>{profile.name}</div>
          )}
          {editing ? (
            <input
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="Descrizione breve (opzionale)"
              style={{ ...input, marginTop: 6 }}
            />
          ) : (
            <>
              {profile.description && (
                <div style={{ fontSize: 12, color: tc.textMuted, marginTop: 2 }}>
                  {profile.description}
                </div>
              )}
              {profile.sourceTemplateKey && (
                <div style={{ fontSize: 11, color: tc.textMuted, marginTop: 2, opacity: 0.55 }}>
                  sorgente: {profile.sourceTemplateKey}
                </div>
              )}
            </>
          )}
        </div>
        <div style={{ display: "flex", gap: 6, flexShrink: 0 }}>
          {editing ? (
            <>
              <button style={btn("ghost")} onClick={() => { setEditing(false); setOpenSection(null); }}>
                Annulla
              </button>
              <button style={btn("primary")} onClick={() => void handleSave()} disabled={saving}>
                {saving ? "Salvo…" : "Salva"}
              </button>
            </>
          ) : (
            <>
              <button style={btn("ghost")} onClick={() => setEditing(true)}>✎ Modifica</button>
              <button style={btn("danger")} onClick={() => void handleDelete()} disabled={deleting}>
                {deleting ? "…" : "Elimina"}
              </button>
            </>
          )}
        </div>
      </div>

      {/* Sezione: System Prompt */}
      <div
        style={sectionHeader("System Prompt", "prompt")}
        onClick={() => setOpenSection(openSection === "prompt" ? null : "prompt")}
      >
        <span>System Prompt</span>
        <span style={{ opacity: 0.6 }}>{openSection === "prompt" ? "▲" : "▼"}</span>
      </div>
      {openSection === "prompt" && (
        <div style={{ padding: "12px 14px" }}>
          {editing ? (
            <textarea
              value={systemPrompt}
              onChange={(e) => setSystemPrompt(e.target.value)}
              placeholder="Istruzioni di comportamento per l'AI…"
              rows={9}
              style={{
                ...input,
                resize: "vertical",
                fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
                fontSize: 12,
              }}
            />
          ) : (
            <pre
              style={{
                margin: 0,
                fontSize: 12,
                color: tc.textSecondary ?? tc.textMuted,
                background: `${tc.accent}08`,
                borderRadius: 6,
                padding: "10px 12px",
                fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
                whiteSpace: "pre-wrap",
                maxHeight: 200,
                overflow: "auto",
              }}
            >
              {profile.systemPrompt || <em style={{ opacity: 0.4 }}>Nessun system prompt</em>}
            </pre>
          )}
        </div>
      )}

      {/* Sezione: Supporto AI */}
      <div
        style={sectionHeader("Supporto AI", "ai")}
        onClick={() => setOpenSection(openSection === "ai" ? null : "ai")}
      >
        <span>Supporto AI — provider, modello, automazione</span>
        <span style={{ opacity: 0.6 }}>{openSection === "ai" ? "▲" : "▼"}</span>
      </div>
      {openSection === "ai" && (
        <div style={{ padding: "12px 14px", display: "flex", flexDirection: "column", gap: 10 }}>
          {editing ? (
            <>
              <div style={{ display: "flex", gap: 10 }}>
                <div style={{ flex: 1 }}>
                  <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 4 }}>Provider</div>
                  <input
                    value={defaultProvider}
                    onChange={(e) => setDefaultProvider(e.target.value)}
                    placeholder="es. anthropic, openai, ollama…"
                    style={input}
                  />
                </div>
                <div style={{ flex: 1 }}>
                  <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 4 }}>Modello</div>
                  <input
                    value={defaultModel}
                    onChange={(e) => setDefaultModel(e.target.value)}
                    placeholder="es. claude-opus-4-7, gpt-4o…"
                    style={input}
                  />
                </div>
              </div>
              <div>
                <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 6 }}>
                  Modalità automazione
                </div>
                <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                  {AUTOMATION_OPTIONS.map((opt) => (
                    <label
                      key={opt.value}
                      style={{
                        display: "flex",
                        alignItems: "center",
                        gap: 8,
                        fontSize: 13,
                        cursor: "pointer",
                        padding: "5px 8px",
                        borderRadius: 6,
                        border: `1px solid ${defaultAutomation === opt.value ? tc.accent : tc.border}`,
                        background: defaultAutomation === opt.value ? `${tc.accent}0d` : "transparent",
                      }}
                    >
                      <input
                        type="radio"
                        name={`automation-${profile.id}`}
                        value={opt.value}
                        checked={defaultAutomation === opt.value}
                        onChange={() => setDefaultAutomation(opt.value)}
                        style={{ accentColor: tc.accent }}
                      />
                      <span style={{ color: tc.text }}>{opt.label}</span>
                    </label>
                  ))}
                  <label
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 8,
                      fontSize: 13,
                      cursor: "pointer",
                      padding: "5px 8px",
                      borderRadius: 6,
                      border: `1px solid ${!defaultAutomation ? tc.accent : tc.border}`,
                      background: !defaultAutomation ? `${tc.accent}0d` : "transparent",
                    }}
                  >
                    <input
                      type="radio"
                      name={`automation-${profile.id}`}
                      value=""
                      checked={!defaultAutomation}
                      onChange={() => setDefaultAutomation("")}
                      style={{ accentColor: tc.accent }}
                    />
                    <span style={{ color: tc.textMuted }}>Predefinito — segue le impostazioni utente</span>
                  </label>
                </div>
              </div>
            </>
          ) : (
            <div style={{ display: "flex", gap: 16, flexWrap: "wrap" }}>
              {(
                [
                  ["Provider", profile.defaultProvider, "Predefinito"],
                  ["Modello", profile.defaultModel, "Predefinito"],
                  [
                    "Automazione",
                    AUTOMATION_OPTIONS.find((o) => o.value === profile.defaultAutomation)?.label.split(" — ")[0],
                    "Predefinita",
                  ],
                ] as const
              ).map(([label, value, placeholder]) => (
                <div key={label}>
                  <div style={{ fontSize: 11, color: tc.textMuted, marginBottom: 2 }}>{label}</div>
                  <div style={{ fontSize: 13, color: tc.text }}>
                    {value || <em style={{ opacity: 0.4 }}>{placeholder}</em>}
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {/* Sezione: Tools MCP */}
      <div
        style={sectionHeader("Tools MCP", "mcp")}
        onClick={() => setOpenSection(openSection === "mcp" ? null : "mcp")}
      >
        <span>Tools MCP — server abilitati per questo profilo</span>
        <span style={{ opacity: 0.6 }}>{openSection === "mcp" ? "▲" : "▼"}</span>
      </div>
      {openSection === "mcp" && (
        <div style={{ padding: "12px 14px" }}>
          <McpSection profileId={profile.id} tc={tc} />
        </div>
      )}
    </div>
  );
}

// ── Tab Utenti (read-only) ───────────────────────────────────────────────────

function UserProfilesTab({ tc }: { tc: ReturnType<typeof useThemeColors> }) {
  const [profiles, setProfiles] = useState<(UserProfile & { userEmail?: string })[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    adminListUserProfiles()
      .then((d) => setProfiles(d.profiles ?? []))
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  if (loading) return <div style={{ color: tc.textMuted, fontSize: 13 }}>Caricamento…</div>;
  if (error) return <div style={{ color: tc.error, fontSize: 13 }}>Errore: {error}</div>;
  if (profiles.length === 0) {
    return (
      <div style={{ color: tc.textMuted, fontSize: 13, fontStyle: "italic" }}>
        Nessun profilo personalizzato creato dagli utenti.
      </div>
    );
  }

  const byUser: Record<string, typeof profiles> = {};
  for (const p of profiles) {
    const key = p.userEmail ?? p.userId ?? "—";
    (byUser[key] ??= []).push(p);
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      {Object.entries(byUser).map(([email, userProfiles]) => (
        <div key={email}>
          <div
            style={{
              fontSize: 12,
              fontWeight: 700,
              color: tc.textMuted,
              marginBottom: 8,
              letterSpacing: "0.04em",
              textTransform: "uppercase",
            }}
          >
            {email}
          </div>
          <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            {userProfiles.map((p) => (
              <div
                key={p.id}
                style={{
                  border: `1px solid ${tc.border}`,
                  borderRadius: 8,
                  background: tc.bgCard,
                  padding: "10px 14px",
                  display: "flex",
                  alignItems: "center",
                  gap: 10,
                }}
              >
                <span style={{ fontSize: 18 }}>{p.avatarEmoji}</span>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div style={{ fontWeight: 600, fontSize: 13, color: tc.text }}>{p.name}</div>
                  {p.description && (
                    <div style={{ fontSize: 12, color: tc.textMuted }}>{p.description}</div>
                  )}
                </div>
                <div style={{ display: "flex", gap: 6, fontSize: 11, color: tc.textMuted }}>
                  {p.defaultModel && (
                    <span
                      style={{
                        background: `${tc.border}55`,
                        borderRadius: 4,
                        padding: "2px 6px",
                      }}
                    >
                      {p.defaultModel}
                    </span>
                  )}
                  {p.defaultAutomation && (
                    <span
                      style={{
                        background: `${tc.border}55`,
                        borderRadius: 4,
                        padding: "2px 6px",
                      }}
                    >
                      {p.defaultAutomation}
                    </span>
                  )}
                  {p.isDefault && (
                    <span
                      style={{
                        background: `${tc.accent}22`,
                        color: tc.accent,
                        borderRadius: 4,
                        padding: "2px 6px",
                        fontWeight: 700,
                      }}
                    >
                      default
                    </span>
                  )}
                </div>
              </div>
            ))}
          </div>
        </div>
      ))}
    </div>
  );
}

// ── Pagina principale ────────────────────────────────────────────────────────

export default function AdminProfilesPage() {
  const tc = useThemeColors();
  const { input, btn } = useStyles(tc);
  const [tab, setTab] = useState<"system" | "users">("system");
  const [profiles, setProfiles] = useState<UserProfile[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");
  const [newEmoji, setNewEmoji] = useState("🤖");
  const [newDescription, setNewDescription] = useState("");
  const [newPrompt, setNewPrompt] = useState("");
  const [newProvider, setNewProvider] = useState("");
  const [newModel, setNewModel] = useState("");
  const [newAutomation, setNewAutomation] = useState("");
  const [saving, setSaving] = useState(false);

  const load = async () => {
    setLoading(true);
    try {
      const data = await adminListProfiles();
      setProfiles(data.profiles ?? []);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => { void load(); }, []);

  const handleSave = async (id: string, data: Partial<CreateProfilePayload>) => {
    await adminUpdateProfile(id, data);
    await load();
  };

  const handleDelete = async (id: string) => {
    await adminDeleteProfile(id);
    setProfiles((prev) => prev.filter((p) => p.id !== id));
  };

  const handleCreate = async () => {
    if (!newName.trim()) return;
    setSaving(true);
    try {
      await adminCreateProfile({
        name: newName.trim(),
        avatarEmoji: newEmoji,
        description: newDescription.trim() || undefined,
        systemPrompt: newPrompt.trim(),
        defaultProvider: newProvider.trim() || undefined,
        defaultModel: newModel.trim() || undefined,
        defaultAutomation: newAutomation || undefined,
      });
      setNewName(""); setNewEmoji("🤖"); setNewDescription(""); setNewPrompt("");
      setNewProvider(""); setNewModel(""); setNewAutomation("");
      setCreating(false);
      await load();
    } finally {
      setSaving(false);
    }
  };

  const tabStyle = (active: boolean): React.CSSProperties => ({
    padding: "7px 18px",
    fontSize: 13,
    fontWeight: active ? 700 : 500,
    color: active ? tc.accent : tc.textMuted,
    borderTop: "none",
    borderLeft: "none",
    borderRight: "none",
    borderBottom: `2px solid ${active ? tc.accent : "transparent"}`,
    cursor: "pointer",
    background: "transparent",
    fontFamily: "inherit",
  });

  return (
    <div style={{ maxWidth: 820, margin: "0 auto", padding: "24px 16px" }}>
      {/* Header */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          marginBottom: 20,
        }}
      >
        <AdminPageHeader
        title="Profili"
        description="Profili AI di sistema (condivisi) e profili personalizzati degli utenti."
      />
        {tab === "system" && (
          <button
            style={{
              ...btn("primary"),
              padding: "6px 16px",
              fontSize: 13,
            }}
            onClick={() => setCreating((v) => !v)}
          >
            {creating ? "Annulla" : "+ Nuovo profilo"}
          </button>
        )}
      </div>

      {/* Tabs */}
      <div
        style={{
          display: "flex",
          borderBottom: `1px solid ${tc.border}`,
          marginBottom: 20,
          gap: 0,
        }}
      >
        <button style={tabStyle(tab === "system")} onClick={() => setTab("system")}>
          Di sistema
        </button>
        <button style={tabStyle(tab === "users")} onClick={() => setTab("users")}>
          Utenti
        </button>
      </div>

      {/* Tab: profili di sistema */}
      {tab === "system" && (
        <>
          {/* Form creazione */}
          {creating && (
            <div
              style={{
                border: `1px solid ${tc.accent}`,
                borderRadius: 10,
                background: tc.bgCard,
                padding: "16px",
                marginBottom: 20,
                display: "flex",
                flexDirection: "column",
                gap: 10,
              }}
            >
              <div style={{ fontWeight: 700, fontSize: 14, color: tc.text }}>
                Nuovo profilo di sistema
              </div>
              <div style={{ display: "flex", gap: 8 }}>
                <select
                  value={newEmoji}
                  onChange={(e) => setNewEmoji(e.target.value)}
                  style={{ ...input, width: 52, textAlign: "center", fontSize: 18, padding: "3px" }}
                >
                  {EMOJI_OPTIONS.map((e) => <option key={e} value={e}>{e}</option>)}
                </select>
                <input
                  value={newName}
                  onChange={(e) => setNewName(e.target.value)}
                  placeholder="Nome profilo *"
                  style={{ ...input, flex: 1, fontWeight: 600 }}
                />
              </div>
              <input
                value={newDescription}
                onChange={(e) => setNewDescription(e.target.value)}
                placeholder="Descrizione breve (opzionale)"
                style={input}
              />
              <textarea
                value={newPrompt}
                onChange={(e) => setNewPrompt(e.target.value)}
                placeholder="System prompt — istruzioni di comportamento per l'AI"
                rows={6}
                style={{
                  ...input,
                  resize: "vertical",
                  fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
                  fontSize: 12,
                }}
              />
              <div style={{ fontSize: 12, fontWeight: 600, color: tc.textMuted, marginTop: 4 }}>
                Supporto AI (opzionale)
              </div>
              <div style={{ display: "flex", gap: 8 }}>
                <input
                  value={newProvider}
                  onChange={(e) => setNewProvider(e.target.value)}
                  placeholder="Provider (es. anthropic)"
                  style={{ ...input, flex: 1 }}
                />
                <input
                  value={newModel}
                  onChange={(e) => setNewModel(e.target.value)}
                  placeholder="Modello (es. claude-opus-4-7)"
                  style={{ ...input, flex: 1 }}
                />
              </div>
              <select
                value={newAutomation}
                onChange={(e) => setNewAutomation(e.target.value)}
                style={input}
              >
                <option value="">Automazione — predefinita</option>
                {AUTOMATION_OPTIONS.map((o) => (
                  <option key={o.value} value={o.value}>{o.label.split(" — ")[0]}</option>
                ))}
              </select>
              <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
                <button style={btn("ghost")} onClick={() => setCreating(false)}>Annulla</button>
                <button
                  style={btn("primary")}
                  onClick={() => void handleCreate()}
                  disabled={saving || !newName.trim()}
                >
                  {saving ? "Creo…" : "Crea profilo"}
                </button>
              </div>
            </div>
          )}

          {loading ? (
            <div style={{ color: tc.textMuted, fontSize: 13 }}>Caricamento…</div>
          ) : error ? (
            <div style={{ color: tc.error, fontSize: 13 }}>Errore: {error}</div>
          ) : profiles.length === 0 ? (
            <div style={{ color: tc.textMuted, fontSize: 13, fontStyle: "italic" }}>
              Nessun profilo di sistema.
            </div>
          ) : (
            <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
              {profiles.map((p) => (
                <ProfileCard key={p.id} profile={p} onSave={handleSave} onDelete={handleDelete} />
              ))}
            </div>
          )}
        </>
      )}

      {/* Tab: profili utenti */}
      {tab === "users" && <UserProfilesTab tc={tc} />}
    </div>
  );
}
