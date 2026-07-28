"use client";

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
  type ClipboardEvent,
  type RefObject,
} from "react";
import type { ChatAttachment } from "../../lib/api-client";
import type { useThemeColors } from "../../lib/theme";
import { IconButton } from "../icon-button";
import { AutoWidthSelect } from "../auto-width-select";
import { rowFitsInline } from "../shell/panel-sizing-logic";
import {
  forceButtonView,
  isProviderPinned,
  providerSelectTitle,
  PROVIDER_AUTO,
} from "./provider-choice-logic";

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

  // La barra dei controlli sta su UNA riga sempre: quando lo spazio non basta le
  // etichette lasciano il posto ai pittogrammi, invece di mandare a capo il
  // gruppo di destra (Send/Stop finiva su una seconda riga, sotto tutto il resto).
  //
  // Misura, non soglia (regola O): quanto e' larga la barra distesa dipende dalla
  // lingua e dai controlli condizionali (il pulsante Forza, il select del modello e
  // l'avviso "pin non rispettato" compaiono solo in certi stati), quindi una
  // costante in px sarebbe giusta per una configurazione e sbagliata per le altre.
  // La REGOLA del confronto e' il punto unico rowFitsInline (regola L), lo stesso
  // che decide riga-o-popover per la testata della chat.
  //
  // `largDistesaRef` esiste perche' la misura utile e' sempre quella della forma
  // DISTESA, e da compatti non e' piu' leggibile dal DOM (la riga renderizzata e'
  // l'altra). La si registra mentre si e' distesi e la si riusa come termine di
  // paragone: e' un valore misurato sul rendering vero, solo in un istante
  // precedente. Chiedere invece "la forma compatta ci sta?" darebbe sempre di si',
  // e la barra rimbalzerebbe fra le due forme a ogni frame.
  const barraHostRef = useRef<HTMLDivElement>(null);
  const barraRigaRef = useRef<HTMLDivElement>(null);
  const largDistesaRef = useRef(0);
  const [barraDistesa, setBarraDistesa] = useState(true);

  const misuraBarra = useCallback(() => {
    const host = barraHostRef.current;
    const riga = barraRigaRef.current;
    if (!host || !riga) return;
    // Da distesi la riga trabocca dall'host (nowrap + figli che non cedono):
    // scrollWidth e' la sua larghezza naturale. Da compatti sarebbe la larghezza
    // della forma compatta, che non risponde alla domanda: non si aggiorna.
    if (barraDistesa) largDistesaRef.current = riga.scrollWidth;
    setBarraDistesa((corrente) =>
      rowFitsInline(host.clientWidth, largDistesaRef.current, corrente),
    );
  }, [barraDistesa]);

  // A ogni render (il pannello cambia larghezza per il divisore o il viewport, e
  // i controlli condizionali appaiono e spariscono): rimisura sul DOM aggiornato.
  // setBarraDistesa fa bail-out se il verdetto non cambia, quindi non innesca loop.
  useLayoutEffect(() => {
    misuraBarra();
  });

  // Rete per i resize puramente CSS, che non passano da un render di questo albero.
  useEffect(() => {
    const host = barraHostRef.current;
    window.addEventListener("resize", misuraBarra);
    const observer = new ResizeObserver(misuraBarra);
    if (host) observer.observe(host);
    return () => {
      window.removeEventListener("resize", misuraBarra);
      observer.disconnect();
    };
  }, [misuraBarra]);

  // Compatto = la barra non ci sta, OPPURE il pannello e' gia' cosi' stretto che
  // `compact` vale per tutto il composer. I due non si escludono: il secondo e' una
  // proprieta' del pannello, il primo di questa riga.
  const barraCompatta = !barraDistesa || compact;

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
  // Solo "Auto" ha una forma breve: il NOME del provider e' l'informazione che si
  // sta guardando quando se ne sceglie uno a mano, e ridurlo a una sigla
  // vanificherebbe la scelta. "Auto" invece e' lo stato di riposo, e il fulmine lo
  // dice per intero.
  const PROVIDER_OPTIONS = providerValues.map((value) =>
    value === "auto"
      ? { value, label: "⚡ Auto", shortLabel: "⚡" }
      : { value, label: providerLabel(value) },
  );

  const selectStyle = {
    borderRadius: 999,
    border: `1px solid ${tc.border}`,
    background: tc.bgCard,
    color: tc.textSecondary,
    padding: barraCompatta ? "3px 6px" : "4px 8px",
    fontSize: barraCompatta ? 10 : 11,
    fontFamily: "inherit",
    cursor: "pointer",
    minWidth: 0,
  } as const;

  // Il dropdown dice QUALE provider, il pulsante "Forza" dice QUANTO vincola.
  // Sono due stati distinti e la barra li mostra distinti: una selezione senza
  // "Forza" e' una preferenza (il routing puo' cambiare fornitore), col pulsante
  // attivo e' un vincolo duro. Punto unico della distinzione:
  // provider-choice-logic.ts, lo stesso che decide cosa viaggia sul wire — cosi'
  // il colore del bordo non puo' dire una cosa e la richiesta farne un'altra.
  const isProviderChosen = selectedProvider !== PROVIDER_AUTO;
  const isProviderPinnedNow = isProviderPinned(selectedProvider, forceProvider);
  const forceButton = forceButtonView(selectedProvider, forceProvider, automationMode);
  const showAutomationRunMismatch =
    !!runAutomationMode &&
    runAutomationMode !== automationMode &&
    isAgentRunning;
  const automationTitle = showAutomationRunMismatch
    ? `Run in corso avviato in modalita' "${runAutomationMode}" — il dropdown (${automationMode}) vale solo per i prossimi messaggi.`
    : "Automazione: Studio = solo lettura, Conferma = chiede approvazione prima di modifiche, Automatico = esegue senza fermarsi";
  // "override -> fallback" segnala che il run NON sta rispettando la scelta.
  // Vale solo col PIN: con la sola preferenza un provider diverso e' il
  // comportamento promesso, non un'anomalia — segnalarlo come tale sarebbe
  // gridare al lupo a ogni fallback riuscito.
  const showOverrideMismatch =
    isProviderPinnedNow &&
    !!runProvider &&
    runProvider !== selectedProvider &&
    isAgentRunning;
  const showModelMismatch =
    isProviderPinnedNow &&
    selectedModel !== "auto" &&
    !!runModel &&
    runModel !== selectedModel &&
    isAgentRunning;

  // Le forme brevi devono restare distinguibili FRA LORO: sono lo stato in cui la
  // barra vive quando la colonna e' stretta, e un pittogramma uguale per due modi
  // diversi renderebbe invisibile la differenza fra "chiede conferma" ed "esegue
  // senza fermarsi". Il testo per esteso resta nella tendina e nel title.
  const AUTOMATION_OPTIONS = [
    { value: "study", label: "Studio", shortLabel: "📖" },
    { value: "confirm", label: "Conferma", shortLabel: "✋" },
    { value: "automatic", label: "Automatico", shortLabel: "▶" },
  ] as const;

  // L'occhio dice "supervisore", il segno accanto dice QUANTO spesso guarda: senza
  // quel secondo carattere i quattro modi collasserebbero nello stesso simbolo.
  const SUPERVISOR_OPTIONS = [
    { value: "none", label: "👁 Supervisor off", shortLabel: "👁∅" },
    { value: "anomaly", label: "👁 Su anomalia", shortLabel: "👁!" },
    { value: "interleaved", label: "👁 Ogni 5 step", shortLabel: "👁5" },
    { value: "continuous", label: "continuo", shortLabel: "👁∞" },
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
          // UNA riga a riposo: il composta sta in fondo alla colonna della chat,
          // quindi ogni riga che occupa qui e' una riga in meno di conversazione
          // visibile. Resta ridimensionabile a mano (resize: vertical) e cresce
          // comunque fino a maxHeight quando il testo lo richiede: si paga lo
          // spazio solo quando serve davvero.
          rows={1}
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
            minHeight: compact ? 20 : 24,
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
          ref={barraHostRef}
          style={{
            display: "flex",
            alignItems: "center",
            marginTop: compact ? 4 : 6,
            // L'host da' la misura dello spazio disponibile (clientWidth) e taglia
            // il traboccamento della riga distesa nell'istante in cui viene
            // misurata, prima che il layout effect la faccia collassare.
            minWidth: 0,
            overflow: "hidden",
          }}
        >
        <div
          ref={barraRigaRef}
          style={{
            display: "flex",
            alignItems: "center",
            gap: barraCompatta ? 4 : 8,
            // Mai a capo: e' il punto di tutta questa misura. Con "wrap" il gruppo
            // di destra scivolava su una seconda riga invece di far stringere i
            // controlli.
            flexWrap: "nowrap",
            width: "100%",
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
                padding: barraCompatta ? "3px 7px" : "4px 10px",
                fontSize: barraCompatta ? 10 : 11,
                fontFamily: "inherit",
                cursor: "pointer",
                fontWeight: activeMemoryCount > 0 ? 600 : 400,
                display: "flex",
                alignItems: "center",
                gap: barraCompatta ? 3 : 5,
                // I controlli non cedono larghezza: e' cio' che rende `scrollWidth`
                // la larghezza NATURALE della riga e non una gia' compressa.
                flexShrink: 0,
                whiteSpace: "nowrap",
              }}
            >
              {barraCompatta ? "🧠" : "Memoria"}
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
            breve={barraCompatta}
            title={providerSelectTitle(selectedProvider, forceProvider, automationMode)}
            style={{
              ...selectStyle,
              // Arancione = vincolo duro. Una preferenza resta evidenziata (e'
              // una scelta attiva) ma con l'accento, non col colore che nella
              // barra significa "questo non si negozia".
              border: `1px solid ${isProviderPinnedNow ? "#f97316" : isProviderChosen ? tc.accent : tc.border}`,
              background: isProviderPinnedNow ? "#f9731612" : isProviderChosen ? `${tc.accent}12` : tc.bgCard,
              color: isProviderPinnedNow ? "#f97316" : isProviderChosen ? tc.accent : tc.textSecondary,
              fontWeight: isProviderChosen ? 600 : 400,
            }}
          />
          {isProviderChosen && (
            <button
              type="button"
              onClick={() => onForceProviderChange(!forceProvider)}
              title={forceButton.title}
              style={{
                ...selectStyle,
                border: `1px solid ${isProviderPinnedNow ? "#f97316" : tc.border}`,
                background: isProviderPinnedNow ? "#f9731612" : tc.bgCard,
                color: isProviderPinnedNow ? "#f97316" : tc.textSecondary,
                fontWeight: isProviderPinnedNow ? 700 : 500,
                flexShrink: 0,
                whiteSpace: "nowrap",
              }}
            >
              {forceButton.label}
            </button>
          )}
          {/* Senza forma breve, di proposito: il nome del modello e' l'unica cosa
              che questo controllo dice, e compare solo col pin attivo (raro).
              Ridurlo a un simbolo lascerebbe l'utente senza sapere su cosa ha
              pinnato — resta esteso e semmai e' la barra a stringersi altrove. */}
          {isProviderPinnedNow && (
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
              title="Il run in corso non sta rispettando il pin. Cause tipiche: e' partito prima che tu pinnassi questo provider, oppure il modello pinnato non era disponibile."
              style={{
                ...selectStyle,
                border: "1px solid #ef4444",
                background: "rgba(239,68,68,0.10)",
                color: "#ef4444",
                fontWeight: 700,
                flexShrink: 0,
                whiteSpace: "nowrap",
              }}
            >
              {/* In compatto resta il solo segnale d'allarme: e' un avviso, il
                  dettaglio sta nel title (che qui e' gia' esteso). */}
              {barraCompatta ? "⚠" : "⚠ pin non rispettato"}
            </span>
          )}
          <AutoWidthSelect
            value={automationMode}
            options={AUTOMATION_OPTIONS}
            onChange={(value) => onAutomationModeChange(value as "study" | "confirm" | "automatic")}
            title={automationTitle}
            ariaLabel="Modalita' automazione"
            breve={barraCompatta}
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
            breve={barraCompatta}
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
          {/* Il gruppo che DEVE restare raggiungibile: e' quello che finiva su una
              seconda riga. `flexShrink: 0` lo mette al riparo anche quando la
              misura sbaglia per un frame (es. il primo render prima del layout
              effect): meglio troncare un controllo a sinistra che il pulsante
              d'invio. */}
          <div style={{ marginLeft: "auto", display: "inline-flex", alignItems: "center", gap: 8, flexShrink: 0 }}>
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
                {/* In compatto il conteggio senza la parola: "3" dentro la pillola
                    accanto allo Stop resta leggibile, e il title spiega. */}
                {barraCompatta ? pendingCount : `${pendingCount} in coda`}
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
                  whiteSpace: "nowrap",
                }}
              >
                <svg width="12" height="12" viewBox="0 0 12 12" fill="currentColor">
                  <rect x="1" y="1" width="10" height="10" rx="2"/>
                </svg>
                {/* Il quadrato basta a dire "ferma": e' l'icona universale, ed e'
                    gia' disegnata qui sopra. */}
                {!barraCompatta && "Stop"}
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
      </div>
      {attachmentError && (
        <div style={{ color: tc.error, fontSize: 12 }}>{attachmentError}</div>
      )}
    </form>
  );
}
