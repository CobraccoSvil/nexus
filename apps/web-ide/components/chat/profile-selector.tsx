"use client";

import { useState } from "react";
import type { UserProfile } from "../../lib/api-client";
import { DEFAULT_PROFILE_ID } from "../../lib/use-profiles";

interface ProfileSelectorProps {
  profiles: UserProfile[];
  selectedProfileId: string;
  onSelect: (id: string) => void;
  onCreateNew: () => void;
  /** Se fornito, mostra il pulsante "📌 Default progetto" e permette di salvare */
  projectId?: string;
  projectDefaultProfileId?: string | null;
  onSetProjectDefault?: (profileId: string | null) => Promise<void>;
  /** Callback per salvare le modifiche al system_prompt di un profilo utente */
  onUpdateProfile?: (id: string, systemPrompt: string) => Promise<void>;
  /** Callback per fare il fork di un profilo di sistema */
  onForkProfile?: (id: string) => Promise<void>;
  style?: React.CSSProperties;
}

export function ProfileSelector({
  profiles,
  selectedProfileId,
  onSelect,
  onCreateNew,
  projectId,
  projectDefaultProfileId,
  onSetProjectDefault,
  onUpdateProfile,
  onForkProfile,
  style,
}: ProfileSelectorProps) {
  const [expanded, setExpanded] = useState(false);
  const [editingPrompt, setEditingPrompt] = useState<string | null>(null);
  const [savingDefault, setSavingDefault] = useState(false);
  const [savingPrompt, setSavingPrompt] = useState(false);
  const [forking, setForking] = useState(false);

  const selectedProfile = profiles.find((p) => p.id === selectedProfileId) ?? null;
  const isProjectDefault = !!projectDefaultProfileId && projectDefaultProfileId === selectedProfileId;
  const isSystem = selectedProfile?.isSystem ?? false;
  const canEdit = !!selectedProfile && !isSystem && !!onUpdateProfile;
  const canFork = !!selectedProfile && isSystem && !!onForkProfile;
  const canSetDefault = !!projectId && !!onSetProjectDefault && selectedProfileId !== "auto" && selectedProfileId !== DEFAULT_PROFILE_ID;

  const baseStyle: React.CSSProperties = {
    display: "inline-flex",
    flexDirection: "column",
    alignItems: "flex-start",
    gap: 4,
    ...style,
  };

  const rowStyle: React.CSSProperties = {
    display: "inline-flex",
    alignItems: "center",
    gap: 4,
    width: "100%",
  };

  const selectStyle: React.CSSProperties = {
    background: "transparent",
    border: "1px solid rgba(128,128,128,0.3)",
    borderRadius: 6,
    padding: "2px 6px",
    fontSize: 12,
    cursor: "pointer",
    color: "inherit",
    fontFamily: "inherit",
    maxWidth: 140,
  };

  const btnStyle: React.CSSProperties = {
    background: "none",
    border: "1px solid rgba(128,128,128,0.3)",
    borderRadius: 6,
    color: "inherit",
    cursor: "pointer",
    fontSize: 12,
    lineHeight: 1,
    padding: "2px 6px",
    fontFamily: "inherit",
    opacity: 0.7,
  };

  const handleSetDefault = async () => {
    if (!onSetProjectDefault) return;
    setSavingDefault(true);
    try {
      const newId = isProjectDefault ? null : selectedProfileId;
      await onSetProjectDefault(newId);
    } finally {
      setSavingDefault(false);
    }
  };

  const handleFork = async () => {
    if (!onForkProfile || !selectedProfile) return;
    setForking(true);
    try {
      await onForkProfile(selectedProfile.id);
    } finally {
      setForking(false);
    }
  };

  const handleSavePrompt = async () => {
    if (!selectedProfile || !onUpdateProfile || editingPrompt === null) return;
    setSavingPrompt(true);
    try {
      await onUpdateProfile(selectedProfile.id, editingPrompt);
      setExpanded(false);
      setEditingPrompt(null);
    } finally {
      setSavingPrompt(false);
    }
  };

  return (
    <div style={baseStyle}>
      {/* Riga principale: select + pulsanti */}
      <div style={rowStyle}>
        <select
          value={selectedProfileId}
          onChange={(e) => { onSelect(e.target.value); setExpanded(false); setEditingPrompt(null); }}
          style={selectStyle}
          title="Seleziona profilo"
        >
          <option value="auto">✨ Auto</option>
          <option value={DEFAULT_PROFILE_ID}>Default</option>
          {/* Profili utente */}
          {profiles.filter((p) => !p.isSystem).length > 0 && (
            <optgroup label="I miei profili">
              {profiles.filter((p) => !p.isSystem).map((p) => (
                <option key={p.id} value={p.id}>
                  {p.avatarEmoji} {p.name}{projectDefaultProfileId === p.id ? " 📌" : ""}
                </option>
              ))}
            </optgroup>
          )}
          {/* Profili di sistema */}
          {profiles.filter((p) => p.isSystem).length > 0 && (
            <optgroup label="Profili di sistema">
              {profiles.filter((p) => p.isSystem).map((p) => (
                <option key={p.id} value={p.id}>
                  🔒 {p.avatarEmoji} {p.name}{projectDefaultProfileId === p.id ? " 📌" : ""}
                </option>
              ))}
            </optgroup>
          )}
        </select>

        {/* Bottone espandi editor (solo profili utente) */}
        {canEdit && (
          <button
            type="button"
            onClick={() => {
              if (!expanded) setEditingPrompt(selectedProfile.systemPrompt);
              setExpanded((v) => !v);
            }}
            style={{ ...btnStyle, opacity: expanded ? 1 : 0.7 }}
            title={expanded ? "Chiudi editor" : "Modifica system prompt"}
          >
            {expanded ? "▾" : "✎"}
          </button>
        )}

        {/* Bottone fork (solo profili di sistema) */}
        {canFork && (
          <button
            type="button"
            onClick={() => void handleFork()}
            disabled={forking}
            style={{ ...btnStyle, opacity: forking ? 0.4 : 0.8 }}
            title="Crea una copia personale modificabile di questo profilo"
          >
            {forking ? "…" : "⑂"}
          </button>
        )}

        {/* Bottone default progetto */}
        {canSetDefault && (
          <button
            type="button"
            onClick={() => void handleSetDefault()}
            disabled={savingDefault}
            style={{
              ...btnStyle,
              opacity: 1,
              color: isProjectDefault ? "#f97316" : "inherit",
              borderColor: isProjectDefault ? "#f9731660" : "rgba(128,128,128,0.3)",
            }}
            title={isProjectDefault ? "Rimuovi come default del progetto" : "Imposta come profilo default per questo progetto"}
          >
            📌
          </button>
        )}

        <button
          type="button"
          onClick={onCreateNew}
          title="Crea o gestisci profili"
          style={btnStyle}
        >
          +
        </button>
      </div>

      {/* Editor inline system_prompt */}
      {expanded && selectedProfile && (
        <div
          style={{
            display: "flex",
            flexDirection: "column",
            gap: 6,
            padding: "8px 10px",
            border: "1px solid rgba(128,128,128,0.25)",
            borderRadius: 8,
            background: "rgba(0,0,0,0.06)",
          }}
        >
          <div style={{ fontSize: 11, opacity: 0.6, fontWeight: 600 }}>
            System prompt — {selectedProfile.avatarEmoji} {selectedProfile.name}
          </div>
          <textarea
            value={editingPrompt ?? ""}
            onChange={(e) => setEditingPrompt(e.target.value)}
            rows={6}
            style={{
              width: "100%",
              boxSizing: "border-box",
              fontSize: 12,
              fontFamily: "var(--font-mono)",
              background: "transparent",
              border: "1px solid rgba(128,128,128,0.3)",
              borderRadius: 6,
              color: "inherit",
              padding: "6px 8px",
              resize: "vertical",
            }}
          />
          <div style={{ display: "flex", gap: 6, justifyContent: "flex-end" }}>
            <button
              type="button"
              onClick={() => { setExpanded(false); setEditingPrompt(null); }}
              style={{ ...btnStyle, fontSize: 11 }}
            >
              Annulla
            </button>
            <button
              type="button"
              onClick={() => void handleSavePrompt()}
              disabled={savingPrompt || editingPrompt === selectedProfile.systemPrompt}
              style={{
                ...btnStyle,
                fontSize: 11,
                opacity: (savingPrompt || editingPrompt === selectedProfile.systemPrompt) ? 0.4 : 1,
                background: "rgba(34,197,94,0.15)",
                borderColor: "rgba(34,197,94,0.4)",
                color: "#22c55e",
              }}
            >
              {savingPrompt ? "Salvo…" : "Salva"}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
