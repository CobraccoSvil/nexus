"use client";

import { useState } from "react";
import type { CreateProfilePayload, UpdateProfilePayload, UserProfile } from "../../lib/api-client";
import { generateSystemPrompt } from "../../lib/api-client";
import { useThemeColors } from "../../lib/theme";

interface ProfileEditorProps {
  profile?: UserProfile;
  allProfiles: UserProfile[];
  onSave: (payload: CreateProfilePayload | UpdateProfilePayload) => Promise<void>;
  onDelete?: () => Promise<void>;
  onSetDefault?: () => Promise<void>;
  onClose: () => void;
}

const AUTOMATION_OPTIONS = [
  { value: "", label: "Eredita dal globale" },
  { value: "confirm", label: "Conferma" },
  { value: "automatic", label: "Automatico" },
  { value: "study", label: "Studio" },
];

const PROVIDER_OPTIONS = [
  { value: "",         label: "Provider globale" },
  { value: "auto",     label: "Auto" },
  { value: "anthropic",label: "Anthropic" },
  { value: "openai",   label: "OpenAI" },
  { value: "google",   label: "Google" },
  { value: "deepseek", label: "DeepSeek" },
  { value: "mistral",  label: "Mistral" },
];

const PROVIDER_MODELS: Record<string, string[]> = {
  anthropic: ["claude-sonnet-4-6", "claude-opus-4-6", "claude-haiku-4-5-20251001", "claude-3-haiku-20240307"],
  openai:    ["gpt-4.1-mini", "gpt-4.1", "gpt-4.1-nano", "o4-mini", "o3", "gpt-4o-mini"],
  google:    ["gemini-2.5-flash", "gemini-2.5-pro", "gemini-2.5-flash-lite", "gemini-2.0-flash", "gemini-1.5-flash"],
  deepseek:  ["deepseek-chat", "deepseek-reasoner", "deepseek-coder"],
  mistral:   ["mistral-small-4", "mistral-large-2411", "codestral-latest", "open-mistral-nemo"],
  auto: [],
  "": [],
};

export function ProfileEditor({
  profile,
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  allProfiles,
  onSave,
  onDelete,
  onSetDefault,
  onClose,
}: ProfileEditorProps) {
  const tc = useThemeColors();
  const isEdit = Boolean(profile);

  const [name, setName] = useState(profile?.name ?? "");
  const [emoji, setEmoji] = useState(profile?.avatarEmoji ?? "🤖");
  const [description, setDescription] = useState(profile?.description ?? "");
  const [systemPrompt, setSystemPrompt] = useState(profile?.systemPrompt ?? "");
  const [defaultProvider, setDefaultProvider] = useState(profile?.defaultProvider ?? "");
  const [defaultModel, setDefaultModel] = useState(profile?.defaultModel ?? "");
  const [defaultAutomation, setDefaultAutomation] = useState(profile?.defaultAutomation ?? "");
  const [isSaving, setIsSaving] = useState(false);
  const [isDeleting, setIsDeleting] = useState(false);
  const [isGenerating, setIsGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const availableModels = PROVIDER_MODELS[defaultProvider] ?? [];

  const handleProviderChange = (value: string) => {
    setDefaultProvider(value);
    // Auto-seleziona il primo modello del provider
    const models = PROVIDER_MODELS[value] ?? [];
    setDefaultModel(models[0] ?? "");
  };

  const handleGeneratePrompt = async () => {
    if (!name.trim()) { setError("Inserisci prima il nome del profilo"); return; }
    setError(null);
    setIsGenerating(true);
    try {
      const provider = defaultProvider && defaultProvider !== "auto" ? defaultProvider : "anthropic";
      const result = await generateSystemPrompt(name, description || undefined, provider);
      setSystemPrompt(result.text);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Errore nella generazione");
    } finally {
      setIsGenerating(false);
    }
  };

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim()) { setError("Il nome e' obbligatorio"); return; }
    setError(null);
    setIsSaving(true);
    try {
      await onSave({
        name: name.trim(),
        avatarEmoji: emoji.trim() || "🤖",
        description: description.trim() || undefined,
        systemPrompt: systemPrompt.trim() || undefined,
        defaultProvider: defaultProvider || undefined,
        defaultModel: defaultModel.trim() || undefined,
        defaultAutomation: (defaultAutomation as "study" | "confirm" | "automatic") || undefined,
      });
      onClose();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Errore nel salvataggio");
    } finally {
      setIsSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!onDelete || !confirm("Eliminare il profilo?")) return;
    setIsDeleting(true);
    try {
      await onDelete();
      onClose();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "Errore nell'eliminazione");
    } finally {
      setIsDeleting(false);
    }
  };

  const overlayStyle: React.CSSProperties = {
    position: "fixed",
    inset: 0,
    background: "rgba(0,0,0,0.5)",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    zIndex: 9999,
  };

  const modalStyle: React.CSSProperties = {
    background: tc.bgCard,
    border: `1px solid ${tc.border}`,
    borderRadius: 10,
    padding: 24,
    width: 480,
    maxWidth: "95vw",
    maxHeight: "90vh",
    overflowY: "auto",
    display: "flex",
    flexDirection: "column",
    gap: 14,
  };

  const inputStyle: React.CSSProperties = {
    width: "100%",
    background: tc.bgInput,
    border: `1px solid ${tc.border}`,
    borderRadius: 6,
    color: tc.text,
    fontSize: 13,
    padding: "6px 10px",
    fontFamily: "inherit",
    boxSizing: "border-box",
  };

  const labelStyle: React.CSSProperties = {
    fontSize: 11,
    fontWeight: 600,
    color: tc.textSecondary,
    marginBottom: 4,
    display: "block",
    textTransform: "uppercase",
    letterSpacing: "0.04em",
  };

  return (
    <div style={overlayStyle} onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}>
      <div style={modalStyle} onClick={(e) => e.stopPropagation()}>
        {/* Header */}
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
          <span style={{ fontSize: 15, fontWeight: 700, color: tc.text }}>
            {isEdit ? "Modifica profilo" : "Nuovo profilo"}
          </span>
          <button
            type="button"
            onClick={onClose}
            style={{ background: "none", border: "none", color: tc.textSecondary, cursor: "pointer", fontSize: 18, lineHeight: 1 }}
          >
            x
          </button>
        </div>

        <form onSubmit={handleSubmit} style={{ display: "flex", flexDirection: "column", gap: 12 }}>
          {/* Nome + Emoji */}
          <div style={{ display: "flex", gap: 8 }}>
            <div style={{ flex: "0 0 60px" }}>
              <label style={labelStyle}>Icona</label>
              <input
                value={emoji}
                onChange={(e) => setEmoji(e.target.value)}
                style={{ ...inputStyle, textAlign: "center", fontSize: 20, padding: "4px 6px" }}
                maxLength={4}
                placeholder="🤖"
              />
            </div>
            <div style={{ flex: 1 }}>
              <label style={labelStyle}>Nome *</label>
              <input
                value={name}
                onChange={(e) => setName(e.target.value)}
                style={inputStyle}
                placeholder="Es: Assistente C#, Code Reviewer..."
                required
              />
            </div>
          </div>

          {/* Descrizione */}
          <div>
            <label style={labelStyle}>Descrizione</label>
            <input
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              style={inputStyle}
              placeholder="Breve descrizione del profilo"
            />
          </div>

          {/* System Prompt */}
          <div>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 4 }}>
              <label style={{ ...labelStyle, marginBottom: 0 }}>System Prompt</label>
              <button
                type="button"
                onClick={handleGeneratePrompt}
                disabled={isGenerating}
                title="Genera system prompt con AI in base al nome e descrizione"
                style={{
                  background: isGenerating ? tc.bgInput : `${tc.accent}18`,
                  border: `1px solid ${tc.accent}60`,
                  color: isGenerating ? tc.textMuted : tc.accent,
                  borderRadius: 5,
                  padding: "3px 10px",
                  fontSize: 11,
                  fontWeight: 600,
                  cursor: isGenerating ? "not-allowed" : "pointer",
                  display: "flex",
                  alignItems: "center",
                  gap: 4,
                  fontFamily: "inherit",
                }}
              >
                {isGenerating ? (
                  <>
                    <span style={{ display: "inline-block", animation: "spin 1s linear infinite" }}>⟳</span>
                    Generando...
                  </>
                ) : (
                  <>✨ Genera con AI</>
                )}
              </button>
            </div>
            <textarea
              value={systemPrompt}
              onChange={(e) => setSystemPrompt(e.target.value)}
              style={{ ...inputStyle, minHeight: 120, resize: "vertical" }}
              placeholder="Sei un esperto di C# e .NET. Preferisci pattern SOLID e Clean Architecture. Rispondi sempre con esempi di codice..."
            />
            <div style={{ fontSize: 10, color: tc.textSecondary, marginTop: 3 }}>
              Questo testo viene iniettato all'inizio di ogni conversazione con questo profilo.
            </div>
          </div>

          {/* Provider e Modello */}
          <div style={{ display: "flex", gap: 8 }}>
            <div style={{ flex: 1 }}>
              <label style={labelStyle}>Provider predefinito</label>
              <select
                value={defaultProvider}
                onChange={(e) => handleProviderChange(e.target.value)}
                style={{ ...inputStyle, cursor: "pointer" }}
              >
                {PROVIDER_OPTIONS.map((o) => (
                  <option key={o.value} value={o.value}>{o.label}</option>
                ))}
              </select>
            </div>
            <div style={{ flex: 1 }}>
              <label style={labelStyle}>Modello predefinito</label>
              {availableModels.length > 0 ? (
                <select
                  value={defaultModel}
                  onChange={(e) => setDefaultModel(e.target.value)}
                  style={{ ...inputStyle, cursor: "pointer" }}
                >
                  <option value="">Modello auto</option>
                  {availableModels.map((m) => (
                    <option key={m} value={m}>{m}</option>
                  ))}
                </select>
              ) : (
                <input
                  value={defaultModel}
                  onChange={(e) => setDefaultModel(e.target.value)}
                  style={inputStyle}
                  placeholder="Modello auto"
                />
              )}
            </div>
          </div>

          {/* Automation mode */}
          <div>
            <label style={labelStyle}>Modalita' automazione</label>
            <select
              value={defaultAutomation}
              onChange={(e) => setDefaultAutomation(e.target.value)}
              style={{ ...inputStyle, cursor: "pointer" }}
            >
              {AUTOMATION_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>{o.label}</option>
              ))}
            </select>
          </div>

          {error && (
            <div style={{ background: `${tc.error}18`, border: `1px solid ${tc.error}40`, borderRadius: 6, padding: "8px 12px", color: tc.error, fontSize: 12 }}>
              {error}
            </div>
          )}

          {/* Azioni */}
          <div style={{ display: "flex", gap: 8, justifyContent: "space-between", paddingTop: 4 }}>
            <div style={{ display: "flex", gap: 6 }}>
              {isEdit && onDelete && (
                <button
                  type="button"
                  onClick={handleDelete}
                  disabled={isDeleting}
                  style={{ padding: "6px 14px", borderRadius: 6, border: `1px solid ${tc.error}60`, background: "none", color: tc.error, fontSize: 12, cursor: "pointer" }}
                >
                  {isDeleting ? "..." : "Elimina"}
                </button>
              )}
              {isEdit && onSetDefault && !profile?.isDefault && (
                <button
                  type="button"
                  onClick={async () => { await onSetDefault?.(); onClose(); }}
                  style={{ padding: "6px 14px", borderRadius: 6, border: `1px solid ${tc.border}`, background: "none", color: tc.textSecondary, fontSize: 12, cursor: "pointer" }}
                >
                  Imposta default
                </button>
              )}
            </div>
            <div style={{ display: "flex", gap: 6 }}>
              <button
                type="button"
                onClick={onClose}
                style={{ padding: "6px 14px", borderRadius: 6, border: `1px solid ${tc.border}`, background: "none", color: tc.textSecondary, fontSize: 12, cursor: "pointer" }}
              >
                Annulla
              </button>
              <button
                type="submit"
                disabled={isSaving}
                style={{ padding: "6px 18px", borderRadius: 6, border: "none", background: tc.accent, color: "#fff", fontSize: 12, fontWeight: 600, cursor: isSaving ? "not-allowed" : "pointer" }}
              >
                {isSaving ? "Salvo..." : isEdit ? "Salva" : "Crea"}
              </button>
            </div>
          </div>
        </form>
      </div>
    </div>
  );
}
