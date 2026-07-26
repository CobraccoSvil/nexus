"use client";

import { type FormEvent, type KeyboardEvent, type ClipboardEvent, type RefObject } from "react";
import type { ChatAttachment } from "../../lib/api-client";
import type { useThemeColors } from "../../lib/theme";
import { IconButton } from "../icon-button";
import { AutoWidthSelect } from "../auto-width-select";

type ThemeColors = ReturnType<typeof useThemeColors>;


export interface ComposerProps {
  input: string;
  onInputChange: (value: string) => void;
  attachments: ChatAttachment[];
  onRemoveAttachment: (name: string, sizeBytes: number) => void;
  attachmentError: string | null;
  selectedProvider: string;
  onProviderChange: (value: string) => void;
  forceProvider: boolean;
  onForceProviderChange: (value: boolean) => void;
  selectedModel: string;
  onModelChange: (value: string) => void;
  providerModels: string[];
  /** Provider attivi dal catalog DB (regola G): niente elenco hardcoded.
   *  Vuoto = solo "Auto" (caso vuoto, nessun fallback hardcoded). */
  availableProviders: string[];
  runProvider?: string | null;
  runModel?: string | null;
  /** Modalita' automazione del run ATTIVO (fissata all'avvio). Se diversa dal
   *  dropdown, il run in corso NON eredita il cambio. */
  runAutomationMode?: string | null;
  automationMode: "study" | "confirm" | "automatic";
  onAutomationModeChange: (value: "study" | "confirm" | "automatic") => void;
  supervisorMode: "none" | "anomaly" | "interleaved" | "continuous";
  onSupervisorModeChange: (value: "none" | "anomaly" | "interleaved" | "continuous") => void;
  showMemory: boolean;
  onOpenMemory: () => void;
  activeMemoryCount?: number;
  micSupported: boolean;
  isListening: boolean;
  onToggleMicrophone: () => void;
  isLoading: boolean;
  isAgentRunning?: boolean;
  /** Numero di messaggi in coda (inviati durante un run, verranno processati a fine run). */
  pendingCount?: number;
  onStopAgent?: () => void;
  hasRunningServices?: boolean;
  hasProject: boolean;
  fileInputRef: RefObject<HTMLInputElement | null>;
  onPickFiles: (files: FileList | null) => void;
  onPasteImages: (files: File[]) => void;
  onSubmit: (e: FormEvent) => void;
  tc: ThemeColors;
  t: (key: string) => string;
  /** Compact mode for narrow panels (< 340px) */
  compact?: boolean;
}


export function Composer({
  input,
  onInputChange,
  attachments,
  onRemoveAttachment,
  attachmentError,
  selectedProvider,
  onProviderChange,
  forceProvider,
  onForceProviderChange,
  selectedModel,
  onModelChange,
  providerModels,
  availableProviders,
  runProvider = null,
  runModel = null,
  runAutomationMode = null,
  automationMode,
  onAutomationModeChange,
  supervisorMode,
  onSupervisorModeChange,
  showMemory,
  onOpenMemory,
  activeMemoryCount = 0,
  micSupported,
  isListening,
  onToggleMicrophone,
  isLoading,
  isAgentRunning = false,
  pendingCount = 0,
  onStopAgent,
  hasRunningServices = false,
  hasProject,
  fileInputRef,
  onPickFiles,
  onPasteImages,
  onSubmit,
  tc,
  t,
  compact = false,
}: ComposerProps) {
  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      // Invia con Enter. Durante un AGENT RUN (isAgentRunning) consentiamo
      // comunque l'invio: send() accoda il messaggio nella coda e lo processera'
      // a fine run (senza questo, durante un run il bottone e' "Stop" e Enter era
      // bloccato da isLoading -> impossibile accodare). Blocchiamo solo quando
      // isLoading senza run attivo (es. precheck/POST in volo) per evitare il
      // doppio invio al server.
      if (!isLoading || isAgentRunning) {
        onSubmit(e as unknown as FormEvent);
      }
    }
  };

  const handlePaste = (e: ClipboardEvent<HTMLTextAreaElement>) => {
    const items = e.clipboardData?.items;
    if (!items) return;
    const imageFiles: File[] = [];
    for (const item of Array.from(items)) {
      if (item.type.startsWith("image/")) {
        const file = item.getAsFile();
        if (file) imageFiles.push(file);
      }
    }
    if (imageFiles.length > 0) {
      e.preventDefault();
      onPasteImages(imageFiles);
    }
  };

  // Etichette "carine" per i provider noti: SOLO cosmesi, non governano quali
  // provider esistono (la fonte e' availableProviders dal catalog DB, regola G).
  // Un provider nuovo non mappato appare comunque, con label capitalizzato.
  const PROVIDER_LABELS: Record<string, string> = {
    openai: "OpenAI",
    anthropic: "Anthropic",
    google: "Google",
    deepseek: "DeepSeek",
    mistral: "Mistral",
  };
  const providerLabel = (value: string) =>
    PROVIDER_LABELS[value] ?? value.charAt(0).toUpperCase() + value.slice(1);

  // Opzioni dropdown: "Auto" (routing intelligente, sempre presente) + i provider
  // attivi dal DB. Se il provider selezionato non e' (ancora) nella lista — fetch
  // in corso o preferenza salvata su un provider poi rimosso — lo includiamo
  // comunque per non lasciare il <select> con un value orfano.
  const providerValues = ["auto", ...availableProviders];
  if (selectedProvider !== "auto" && !providerValues.includes(selectedProvider)) {
    providerValues.push(selectedProvider);
  }
  const PROVIDER_OPTIONS = providerValues.map((value) =>
    value === "auto"
      ? { value, label: "⚡ Auto" }
      : { value, label: providerLabel(value) },
  );

  const selectStyle = {
    borderRadius: 999,
    border: `1px solid ${tc.border}`,
    background: tc.bgCard,
    color: tc.textSecondary,
    padding: compact ? "3px 6px" : "4px 8px",
    fontSize: compact ? 10 : 11,
    fontFamily: "inherit",
    cursor: "pointer",
    minWidth: 0,
  } as const;

  // Un provider selezionato (diverso da "auto") e' gia' forzato come override,
  // a prescindere dal toggle "Forza": il dropdown e' la fonte di verita'.
  const isProviderLocked = selectedProvider !== "auto";
  const showAutomationRunMismatch =
    !!runAutomationMode &&
    runAutomationMode !== automationMode &&
    isAgentRunning;
  const automationTitle = showAutomationRunMismatch
    ? `Run in corso avviato in modalita' "${runAutomationMode}" — il dropdown (${automationMode}) vale solo per i prossimi messaggi.`
    : "Automazione: Studio = solo lettura, Conferma = chiede approvazione prima di modifiche, Automatico = esegue senza fermarsi";
  const showOverrideMismatch =
    isProviderLocked &&
    !!runProvider &&
    runProvider !== selectedProvider &&
    isAgentRunning;
  const showModelMismatch =
    isProviderLocked &&
    selectedModel !== "auto" &&
    !!runModel &&
    runModel !== selectedModel &&
    isAgentRunning;

  const AUTOMATION_OPTIONS = [
    { value: "study", label: "Studio" },
    { value: "confirm", label: "Conferma" },
    { value: "automatic", label: "Automatico" },
  ] as const;

  const SUPERVISOR_OPTIONS = [
    { value: "none", label: "👁 Supervisor off" },
    { value: "anomaly", label: "👁 Su anomalia" },
    { value: "interleaved", label: "👁 Ogni 5 step" },
    { value: "continuous", label: "continuo" },
  ] as const;

  const MODEL_OPTIONS = [
    { value: "auto", label: "Modello auto" },
    ...providerModels.map((model) => ({ value: model, label: model })),
  ];

  return (
    <form
      onSubmit={onSubmit}
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 8,
        marginTop: 8,
        flexShrink: 0,
        width: "100%",
        minWidth: 0,
        alignSelf: "stretch",
        boxSizing: "border-box",
      }}
    >
      <input
        ref={fileInputRef}
        type="file"
        multiple
        accept=".txt,.md,.json,.ts,.tsx,.js,.jsx,.rs,.py,.sql,.html,.css,.yml,.yaml,.toml,.xml,.cs,.java,.kt,.go,.php,.sh,.env,.log,text/*,image/*"
        onChange={(e) => onPickFiles(e.target.files)}
        style={{ display: "none" }}
      />
      <div
        style={{
          borderRadius: compact ? 16 : 22,
          border: `1px solid ${tc.border}`,
          background: tc.bgCard,
          padding: compact ? "8px 10px 6px" : "12px 14px 10px",
          boxShadow: "0 6px 18px rgba(0,0,0,0.08)",
          width: "100%",
          minWidth: 0,
          boxSizing: "border-box",
        }}
      >
        <textarea
          value={input}
          onChange={(e) => onInputChange(e.target.value)}
          onKeyDown={handleKeyDown}
          onPaste={handlePaste}
          disabled={!hasProject}
          placeholder={hasProject ? "Chiedi a Nexus..." : "Apri un progetto per iniziare..."}
          rows={2}
          style={{
            width: "100%",
            padding: 0,
            borderRadius: 0,
            border: "none",
            background: "transparent",
            color: tc.text,
            fontSize: compact ? 13 : 14,
            resize: "vertical",
            fontFamily: "inherit",
            minHeight: compact ? 32 : 40,
            maxHeight: compact ? 200 : 340,
            boxSizing: "border-box",
            outline: "none",
            overflowY: "auto",
          }}
        />
        {attachments.length > 0 && (
          <div style={{ display: "flex", flexWrap: "wrap", gap: 6, marginTop: 8 }}>
            {attachments.map((attachment) => {
              const isImage = (attachment.mimeType || "").startsWith("image/");
              const ext = (attachment.name.split(".").pop() || "file").slice(0, 4);
              return (
              <span
                key={`${attachment.name}-${attachment.sizeBytes}`}
                style={{
                  display: "inline-flex",
                  alignItems: "center",
                  gap: 6,
                  padding: "4px 8px",
                  borderRadius: 999,
                  background: tc.bgInput,
                  border: `1px solid ${tc.border}`,
                  fontSize: 11,
                  color: tc.textSecondary,
                }}
              >
                {isImage && attachment.base64Content ? (
                  <img
                    src={`data:${attachment.mimeType};base64,${attachment.base64Content}`}
                    alt={attachment.name}
                    style={{ height: 20, width: 20, borderRadius: 4, objectFit: "cover" }}
                  />
                ) : (
                  <span
                    aria-hidden
                    style={{
                      display: "inline-flex",
                      alignItems: "center",
                      justifyContent: "center",
                      height: 18,
                      minWidth: 18,
                      padding: "0 4px",
                      borderRadius: 3,
                      background: `${tc.accent}22`,
                      color: tc.accent,
                      fontSize: 9,
                      fontWeight: 700,
                      textTransform: "uppercase",
                    }}
                  >
                    {ext}
                  </span>
                )}
                {attachment.name}
                <button
                  type="button"
                  onClick={() => onRemoveAttachment(attachment.name, attachment.sizeBytes)}
                  style={{
                    border: "none",
                    background: "transparent",
                    color: tc.textMuted,
                    cursor: "pointer",
                    padding: 0,
                    fontSize: 11,
                  }}
                >
                  x
                </button>
              </span>
              );
            })}
          </div>
        )}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: compact ? 4 : 8,
            flexWrap: "wrap",
            marginTop: compact ? 4 : 6,
          }}
        >
          {hasProject && (
            <button
              type="button"
              onClick={onOpenMemory}
              title={showMemory ? "Memoria già aperta" : activeMemoryCount > 0 ? `${activeMemoryCount} memori${activeMemoryCount === 1 ? "a attiva" : "e attive"}` : "Gestisci memoria del progetto"}
              style={{
                borderRadius: 999,
                border: `1px solid ${activeMemoryCount > 0 ? tc.accent : tc.border}`,
                background: activeMemoryCount > 0 ? `${tc.accent}18` : tc.bgCard,
                color: activeMemoryCount > 0 ? tc.accent : tc.textSecondary,
                padding: compact ? "3px 7px" : "4px 10px",
                fontSize: compact ? 10 : 11,
                fontFamily: "inherit",
                cursor: "pointer",
                fontWeight: activeMemoryCount > 0 ? 600 : 400,
                display: "flex",
                alignItems: "center",
                gap: compact ? 3 : 5,
              }}
            >
              Memoria
              {activeMemoryCount > 0 && (
                <span style={{
                  background: tc.accent,
                  color: "#fff",
                  borderRadius: 999,
                  fontSize: 9,
                  fontWeight: 700,
                  padding: "1px 5px",
                  lineHeight: 1.4,
                  minWidth: 14,
                  textAlign: "center",
                }}>
                  {activeMemoryCount}
                </span>
              )}
            </button>
          )}
          <AutoWidthSelect
            value={selectedProvider}
            options={PROVIDER_OPTIONS}
            onChange={onProviderChange}
            title={isProviderLocked ? `Provider forzato su ${selectedProvider} — disattiva "Forza" o passa ad Auto per routing intelligente` : "Routing automatico: sceglie il modello migliore per ogni task"}
            style={{
              ...selectStyle,
              border: `1px solid ${isProviderLocked ? "#f97316" : tc.border}`,
              background: isProviderLocked ? "#f9731612" : tc.bgCard,
              color: isProviderLocked ? "#f97316" : tc.textSecondary,
              fontWeight: isProviderLocked ? 600 : 400,
            }}
          />
          {selectedProvider !== "auto" && (
            <button
              type="button"
              onClick={() => onForceProviderChange(!forceProvider)}
              title={forceProvider ? "Override attivo: il provider selezionato viene forzato" : "Override disattivo: il routing può scegliere un provider diverso"}
              style={{
                ...selectStyle,
                border: `1px solid ${forceProvider ? "#f97316" : tc.border}`,
                background: forceProvider ? "#f9731612" : tc.bgCard,
                color: forceProvider ? "#f97316" : tc.textSecondary,
                fontWeight: forceProvider ? 700 : 500,
              }}
            >
              {forceProvider ? "Forza ✓" : "Forza"}
            </button>
          )}
          {selectedProvider !== "auto" && forceProvider && (
            <AutoWidthSelect
              value={selectedModel}
              options={MODEL_OPTIONS}
              onChange={onModelChange}
              ariaLabel="Modello"
              style={{
                ...selectStyle,
                background: tc.bgCard,
                color: tc.textSecondary,
                cursor: "pointer",
              }}
            />
          )}
          {(showOverrideMismatch || showModelMismatch) && (
            <span
              title="La run non sta rispettando l'override. Possibili cause: provider in cooldown/quota, modello non disponibile, fallback del router."
              style={{
                ...selectStyle,
                border: "1px solid #ef4444",
                background: "rgba(239,68,68,0.10)",
                color: "#ef4444",
                fontWeight: 700,
              }}
            >
              ⚠ override → fallback
            </span>
          )}
          <AutoWidthSelect
            value={automationMode}
            options={AUTOMATION_OPTIONS}
            onChange={(value) => onAutomationModeChange(value as "study" | "confirm" | "automatic")}
            title={automationTitle}
            ariaLabel="Modalita' automazione"
            style={{
              ...selectStyle,
              cursor: "pointer",
              ...(showAutomationRunMismatch
                ? {
                    border: "1px solid #f59e0b",
                    background: "rgba(245,158,11,0.10)",
                    color: "#f59e0b",
                    fontWeight: 700,
                  }
                : {}),
            }}
          />
          <AutoWidthSelect
            value={supervisorMode}
            options={SUPERVISOR_OPTIONS}
            onChange={(value) => onSupervisorModeChange(value as "none" | "anomaly" | "interleaved" | "continuous")}
            title="Supervisore AI (monitora e corregge l'agente). Non sostituisce Conferma/Automatico: per saltare le approvazioni usa Automatico."
            ariaLabel="Supervisore"
            style={{
              ...selectStyle,
              border: `1px solid ${supervisorMode !== "none" ? "#8b5cf6" : tc.border}`,
              background: supervisorMode !== "none" ? "#8b5cf611" : tc.bgCard,
              color: supervisorMode !== "none" ? "#8b5cf6" : tc.textSecondary,
            }}
          />
          <IconButton
            label="Allega file"
            onClick={() => fileInputRef.current?.click()}
            size={32}
            style={{ borderRadius: 999, fontSize: 16 }}
          >
            <span aria-hidden="true">+</span>
          </IconButton>
          <IconButton
            label={micSupported ? "Dettatura microfono" : "Microfono non supportato"}
            onClick={onToggleMicrophone}
            disabled={!micSupported}
            active={isListening}
            style={{ borderRadius: 7, fontSize: 13 }}
          >
            {isListening ? "■" : "\uD83C\uDFA4"}
          </IconButton>
          <div style={{ marginLeft: "auto", display: "inline-flex", alignItems: "center", gap: 8 }}>
            {pendingCount > 0 && (
              <span
                title={`${pendingCount} messaggi in coda: verranno inviati automaticamente al termine del run in corso`}
                style={{
                  display: "inline-flex",
                  alignItems: "center",
                  gap: 4,
                  fontSize: 11,
                  fontWeight: 600,
                  color: tc.accent,
                  background: `${tc.accent}14`,
                  border: `1px solid ${tc.accent}40`,
                  borderRadius: 7,
                  padding: "3px 8px",
                  whiteSpace: "nowrap",
                }}
              >
                {pendingCount} in coda
              </span>
            )}
            {isAgentRunning && onStopAgent ? (
              <button
                type="button"
                onClick={onStopAgent}
                title="Interrompi agente"
                style={{
                  border: `1px solid ${tc.error}88`,
                  background: `${tc.error}1a`,
                  color: tc.error,
                  borderRadius: 7,
                  padding: "5px 12px",
                  cursor: "pointer",
                  display: "inline-flex",
                  alignItems: "center",
                  justifyContent: "center",
                  gap: 5,
                  fontSize: 12,
                  fontWeight: 600,
                }}
              >
                <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor">
                  <rect x="1" y="1" width="10" height="10" rx="2"/>
                </svg>
                Stop
              </button>
            ) : (
              <IconButton
                type="submit"
                label={hasRunningServices ? "Ci sono servizi attivi (puoi comunque inviare)" : t("chat.send")}
                disabled={isLoading || (!input.trim() && attachments.length === 0) || !hasProject}
                variant="primary"
                style={{ borderRadius: 7, fontSize: 13 }}
              >
                {isLoading ? "…" : "➤"}
              </IconButton>
            )}
          </div>
        </div>
      </div>
      {attachmentError && (
        <div style={{ color: tc.error, fontSize: 12 }}>{attachmentError}</div>
      )}
    </form>
  );
}
