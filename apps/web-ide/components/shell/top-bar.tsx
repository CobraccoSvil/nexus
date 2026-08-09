"use client";

import { useCallback, useRef, useState } from "react";
import { useDismissOnOutside } from "../../hooks/use-dismiss-on-outside";
import type { useThemeColors } from "../../lib/theme";
import type { UserProjectDetails, UserProjectSummary, WorkbenchLayoutMode } from "../../lib/api-client";
import { ProjectSwitcher } from "../project-switcher";
import { TruncatedText } from "../truncated-text";
import { ConnectionStatusBadge } from "../dispatcher-status";
import {
  StatusDot,
  iconButton,
  providerTitle,
  providerDisplayLabel,
  sortProviderNames,
  summarizeProviderReason,
  type ProviderHealthState,
} from "./shell-helpers";
import { useI18n } from "../../lib/i18n";

type ProviderStatusMap = Record<string, ProviderHealthState>;

/**
 * Indicatore aggregato dei provider AI nella top bar: due soli LED
 * (uno per i provider disponibili, uno per quelli non disponibili). Il click
 * apre un popover con lo stato dettagliato di ogni singolo provider.
 */
function ProviderStatusIndicator({
  tc,
  providerStatus,
}: {
  tc: ReturnType<typeof useThemeColors>;
  providerStatus: ProviderStatusMap;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  const chiudi = useCallback(() => setOpen(false), []);
  useDismissOnOutside(open, containerRef, chiudi);

  const names = sortProviderNames(Object.keys(providerStatus));
  if (names.length === 0) return null;

  const okNames = names.filter((name) => providerStatus[name].ok === true);
  // "non ok" = tutto cio' che non e' confermato disponibile (errori reali,
  // billing/quota, stato sconosciuto).
  const koNames = names.filter((name) => providerStatus[name].ok !== true);
  const hasRealError = koNames.some((name) => providerStatus[name].ok === false && !providerStatus[name].billing);
  const hasBilling = koNames.some((name) => providerStatus[name].billing);
  // Il LED "non ok" riflette lo stato peggiore presente: rosso (errore reale) >
  // giallo (billing/quota) > grigio (solo stato sconosciuto).
  const koStatusDot = hasRealError
    ? { ok: false as const, billing: false }
    : hasBilling
      ? { ok: false as const, billing: true }
      : { ok: null, billing: false };

  // Dettaglio testuale mostrato SOLO per stati non-ok (i provider verdi non hanno
  // riga di dettaglio: il nome + pallino bastano).
  const detailText = (state: ProviderHealthState): string => {
    if (state.ok === null) return "Stato sconosciuto";
    if (state.billing) return summarizeProviderReason(state.reason) ?? "Crediti/quota esauriti";
    return summarizeProviderReason(state.reason) ?? "Non disponibile";
  };

  return (
    <div ref={containerRef} style={{ position: "relative", display: "inline-flex" }}>
      <button
        type="button"
        onClick={() => setOpen((prev) => !prev)}
        title={t("shell.statoProviderAi")}
        aria-label={t("shell.statoProviderAi")}
        aria-expanded={open}
        style={{
          display: "inline-flex",
          alignItems: "center",
          gap: 8,
          padding: "3px 8px",
          borderRadius: 6,
          border: `1px solid ${tc.border}`,
          background: open ? tc.bgHover : "transparent",
          color: tc.textMuted,
          fontSize: 11,
          cursor: "pointer",
        }}
      >
        <span style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
          <StatusDot ok={true} />
          {okNames.length}
        </span>
        <span style={{ display: "inline-flex", alignItems: "center", gap: 4 }}>
          <StatusDot ok={koStatusDot.ok} billing={koStatusDot.billing} />
          {koNames.length}
        </span>
      </button>
      {open && (
        <div
          role="dialog"
          aria-label={t("shell.dettaglioStatoProviderAi")}
          style={{
            position: "absolute",
            top: "calc(100% + 6px)",
            right: 0,
            zIndex: 50,
            minWidth: 240,
            maxWidth: 340,
            maxHeight: 360,
            overflowY: "auto",
            overflowX: "hidden",
            // Il contenitore dei LED in header impone white-space:nowrap (LED su una
            // riga); va resettato qui, altrimenti viene ereditato e il testo del
            // dettaglio non andrebbe a capo (overflow orizzontale).
            whiteSpace: "normal",
            padding: 8,
            borderRadius: 8,
            border: `1px solid ${tc.border}`,
            background: tc.bgCard,
            boxShadow: "0 8px 24px rgba(0,0,0,0.35)",
          }}
        >
          <div
            style={{
              fontSize: 11,
              fontWeight: 700,
              color: tc.textMuted,
              textTransform: "uppercase",
              letterSpacing: "0.05em",
              padding: "2px 6px 6px",
            }}
          >
            {t("shell.providerAi")}
          </div>
          {names.map((name) => {
            const label = providerDisplayLabel(name);
            const state = providerStatus[name];
            return (
              <div
                key={name}
                title={providerTitle(label, state)}
                style={{
                  display: "flex",
                  alignItems: "flex-start",
                  gap: 8,
                  padding: "5px 6px",
                  borderRadius: 6,
                  minWidth: 0,
                }}
              >
                <span style={{ marginTop: 3, flexShrink: 0 }}>
                  <StatusDot ok={state.ok} billing={state.billing} />
                </span>
                <span style={{ minWidth: 0, display: "flex", flexDirection: "column", gap: 1 }}>
                  <span style={{ color: tc.text, fontSize: 12, fontWeight: 600 }}>{label}</span>
                  {/* Provider disponibile (verde): il nome basta, "Disponibile" e' ridondante.
                      Il dettaglio si mostra solo per stati non-ok (billing/errore/sconosciuto).

                      Collassato di DEFAULT: il pannello risponde a "chi e' giu'?", e la
                      risposta e' il pallino accanto al nome. Con due provider in errore il
                      dettaglio esteso occupava meta' popover e spingeva fuori dalla vista
                      i provider sani, che sono quelli su cui si decide dove instradare.
                      `<details>` nativo invece di uno stato React: la persistenza per riga
                      la gestisce il browser, e senza `open` nasce chiuso a ogni apertura --
                      che e' voluto, il pannello e' una lettura di stato, non una sessione. */}
                  {state.ok !== true && (
                    <details style={{ minWidth: 0 }}>
                      <summary
                        style={{
                          color: tc.textMuted,
                          fontSize: 11,
                          cursor: "pointer",
                          listStyle: "revert",
                          userSelect: "none",
                        }}
                      >
                        {t("panels.dettagli")}
                      </summary>
                      <span
                        style={{
                          display: "block",
                          color: tc.textMuted,
                          fontSize: 11,
                          overflowWrap: "anywhere",
                          paddingTop: 2,
                        }}
                      >
                        {detailText(state)}
                      </span>
                    </details>
                  )}
                </span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

export function TopBar({
  tc,
  isMobileViewport,
  isNarrowViewport,
  activeProject,
  projects,
  layoutMode,
  primarySidebarVisible,
  bottomPanelVisible,
  isFullscreen,
  providerStatus,
  onTogglePrimarySidebar,
  onToggleBottomPanel,
  onCycleLayoutMode,
  onToggleFullscreen,
  onSelectProject,
  onRegisterProject,
  onRefreshProjects,
  fixedPanelHidden = false,
}: {
  tc: ReturnType<typeof useThemeColors>;
  isMobileViewport: boolean;
  isNarrowViewport: boolean;
  activeProject: UserProjectDetails | null;
  projects: UserProjectSummary[];
  layoutMode: WorkbenchLayoutMode;
  primarySidebarVisible: boolean;
  bottomPanelVisible: boolean;
  isFullscreen: boolean;
  providerStatus: ProviderStatusMap;
  /** Il pannello a larghezza fissa (editor/SQL) e' a 0px perche' lo spazio non
   *  basta per due colonne. Senza dirlo, sparisce in silenzio e nessun tasto
   *  sembra riportarlo: il layout promette due pannelli e ne mostra uno. */
  fixedPanelHidden?: boolean;
  onTogglePrimarySidebar: () => void;
  onToggleBottomPanel: () => void;
  onCycleLayoutMode: () => void;
  onToggleFullscreen: () => void;
  onSelectProject: (projectId: string) => Promise<void>;
  onRegisterProject: (absolutePath: string, name?: string) => Promise<void>;
  onRefreshProjects: () => Promise<void>;
}) {
  const { t } = useI18n();
  return (
    <header
      style={{
        gridColumn: "1 / 5",
        display: "flex",
        alignItems: "center",
        columnGap: 10,
        padding: "0 12px",
        background: tc.bgHeader,
        borderBottom: `1px solid ${tc.border}`,
        flexWrap: isMobileViewport ? "wrap" : "nowrap",
        rowGap: isMobileViewport ? 6 : 0,
      }}
    >
      <a
        href="/?site"
        title={t("shell.vediSito")}
        style={{
          fontSize: 13,
          letterSpacing: "0.08em",
          color: tc.text,
          fontWeight: 700,
          textDecoration: "none",
          cursor: "pointer",
        }}
      >
        NEXUS
      </a>
      <TruncatedText
        text={activeProject?.name ?? "Nessun progetto"}
        maxWidth={220}
        tc={tc}
        style={{ color: tc.textMuted, fontSize: 12 }}
      />
      <div style={{ width: 1, height: 20, background: tc.border }} />
      <button
        type="button"
        onClick={onTogglePrimarySidebar}
        title={primarySidebarVisible ? "Nascondi primary sidebar" : "Mostra primary sidebar"}
        aria-label={primarySidebarVisible ? "Nascondi primary sidebar" : "Mostra primary sidebar"}
        style={iconButton(tc, false, primarySidebarVisible)}
      >
        ◧
      </button>
      <button
        type="button"
        onClick={onToggleBottomPanel}
        title={bottomPanelVisible ? "Nascondi panel" : "Mostra panel"}
        aria-label={bottomPanelVisible ? "Nascondi panel" : "Mostra panel"}
        style={iconButton(tc, false, bottomPanelVisible)}
      >
        <span style={{ display: "inline-block", transform: "rotate(90deg)" }}>◧</span>
      </button>
      <button
        type="button"
        onClick={onCycleLayoutMode}
        title={
          fixedPanelHidden
            ? `Cambia layout (${layoutMode}) — l'editor e' nascosto: lo spazio non basta per due pannelli`
            : `Cambia layout (${layoutMode})`
        }
        aria-label={`Cambia layout (${layoutMode})`}
        style={iconButton(tc)}
      >
        ⧉
      </button>
      {/* Avviso, non automatismo: il pannello sparito si spiega e offre la sua
          via d'uscita. Ciclare il layout non lo riporta (lo spazio resta quello),
          quindi senza questo l'unico rimedio - chiudere la sidebar - non e'
          deducibile da nessun tasto. Con la sidebar gia' chiusa resta solo
          allargare la finestra, e lo dice. */}
      {fixedPanelHidden && (
        <button
          type="button"
          onClick={primarySidebarVisible ? onTogglePrimarySidebar : undefined}
          disabled={!primarySidebarVisible}
          title={
            primarySidebarVisible
              ? "Editor nascosto: lo spazio non basta per due pannelli. Clicca per chiudere la sidebar e farlo tornare."
              : "Editor nascosto: lo spazio non basta per due pannelli. Allarga la finestra per farlo tornare."
          }
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 6,
            height: 30,
            padding: "0 8px",
            borderRadius: 7,
            border: `1px solid ${tc.warning}`,
            background: "transparent",
            color: tc.warning,
            cursor: primarySidebarVisible ? "pointer" : "default",
            fontSize: 11,
            fontWeight: 600,
            fontFamily: "inherit",
            whiteSpace: "nowrap",
            flexShrink: 0,
          }}
        >
          <span aria-hidden="true">◫</span>
          {!isNarrowViewport && <span>{t("shell.editorNascosto")}</span>}
        </button>
      )}
      <button
        type="button"
        onClick={onToggleFullscreen}
        title={isFullscreen ? "Esci da pieno schermo" : "Vai a pieno schermo"}
        aria-label={isFullscreen ? "Esci da pieno schermo" : "Vai a pieno schermo"}
        style={iconButton(tc, false, isFullscreen)}
      >
        {isFullscreen ? "🗗" : "🗖"}
      </button>
      <div style={{ flex: 1, minWidth: 0, order: isMobileViewport ? 10 : 0 }}>
        <ProjectSwitcher
          projects={projects}
          activeProjectId={activeProject?.id}
          compact={isMobileViewport}
          onSelect={onSelectProject}
          onRegister={onRegisterProject}
          onRefreshProjects={onRefreshProjects}
        />
      </div>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          columnGap: isNarrowViewport ? 8 : 10,
          marginLeft: 8,
          flexShrink: 0,
          order: isMobileViewport ? 11 : 0,
          flexWrap: isMobileViewport ? "wrap" : "nowrap",
          rowGap: isMobileViewport ? 6 : 0,
          whiteSpace: "nowrap",
        }}
        aria-label={t("shell.statoProviderAi")}
      >
        <ConnectionStatusBadge compact={isNarrowViewport} />
        <ProviderStatusIndicator tc={tc} providerStatus={providerStatus} />
      </div>
    </header>
  );
}
