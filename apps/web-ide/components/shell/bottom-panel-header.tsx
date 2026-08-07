"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import type { useThemeColors } from "../../lib/theme";
import type { UserProjectDetails } from "../../lib/api-client";
import type { PanelTab } from "../panels/bottom-panel-manager";
import { QuotaBadge } from "../panels/quota-badge";
import { iconButton, panelTabs } from "./shell-helpers";
import { PanelTabButton } from "./panel-tabs";
import { useI18n } from "../../lib/i18n";

export function BottomPanelHeader({
  tc,
  isMobileViewport,
  activePanelTab,
  activeProject,
  onSelectTab,
}: {
  tc: ReturnType<typeof useThemeColors>;
  isMobileViewport: boolean;
  activePanelTab: PanelTab;
  activeProject: UserProjectDetails | null;
  onSelectTab: (tab: PanelTab) => void;
}) {
  const { t } = useI18n();
  const scorrevoleRef = useRef<HTMLDivElement>(null);
  const [scorribile, setScorribile] = useState({ sinistra: false, destra: false });

  // Quali frecce hanno ancora strada davanti. Va ricalcolato non solo allo
  // scroll: la stessa riga di tab diventa scorrevole o no al variare della
  // larghezza del pannello, quindi serve osservare anche il ridimensionamento.
  const aggiornaFrecce = useCallback(() => {
    const el = scorrevoleRef.current;
    if (!el) return;
    const massimo = el.scrollWidth - el.clientWidth;
    setScorribile({ sinistra: el.scrollLeft > 1, destra: el.scrollLeft < massimo - 1 });
  }, []);

  useEffect(() => {
    const el = scorrevoleRef.current;
    if (!el) return;
    aggiornaFrecce();
    const osservatore = new ResizeObserver(aggiornaFrecce);
    osservatore.observe(el);
    return () => osservatore.disconnect();
  }, [aggiornaFrecce]);

  const scorri = (verso: -1 | 1) => {
    const el = scorrevoleRef.current;
    if (!el) return;
    // Un passo che mostra tab nuovi restando ancorato a qualcosa di gia' visto.
    el.scrollBy({ left: verso * Math.max(120, el.clientWidth * 0.6), behavior: "smooth" });
  };

  const frecciaStyle = (attiva: boolean) => ({
    ...iconButton(tc, !attiva),
    width: 22,
    height: 22,
    borderRadius: 4,
    fontSize: 12,
    flexShrink: 0,
  });

  const mostraFrecce = scorribile.sinistra || scorribile.destra;

  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        borderBottom: `1px solid ${tc.border}`,
        background: tc.bgHeader,
        minWidth: 0,
      }}
    >
      {/* `overflowX: auto` da solo non basta: per regola CSS un asse non-visible
          porta l'altro da `visible` ad `auto`, e la riga di tab si ritrovava una
          scrollbar VERTICALE pur non avendo nulla da scorrere in verticale.
          Entrambi gli assi sono nascosti: qui si scorre con le frecce. */}
      <div
        ref={scorrevoleRef}
        onScroll={aggiornaFrecce}
        style={{
          display: "flex",
          alignItems: "center",
          flex: 1,
          minWidth: 0,
          overflowX: "hidden",
          overflowY: "hidden",
        }}
      >
        {panelTabs.map((tab) => (
          <PanelTabButton
            key={tab.key}
            tab={tab}
            active={activePanelTab === tab.key}
            tc={tc}
            isMobileViewport={isMobileViewport}
            onSelect={() => onSelectTab(tab.key)}
          />
        ))}
      </div>

      {/* Le frecce compaiono solo quando c'e' davvero altro da vedere: a pannello
          largo la riga sta tutta dentro e non serve nessun comando. */}
      {mostraFrecce && (
        <div style={{ display: "flex", alignItems: "center", gap: 2, paddingLeft: 4, flexShrink: 0 }}>
          <button
            type="button"
            onClick={() => scorri(-1)}
            disabled={!scorribile.sinistra}
            title={t("shell.tabPrecedenti")}
            aria-label={t("shell.tabPrecedenti")}
            style={frecciaStyle(scorribile.sinistra)}
          >
            ‹
          </button>
          <button
            type="button"
            onClick={() => scorri(1)}
            disabled={!scorribile.destra}
            title={t("shell.tabSuccessivi")}
            aria-label={t("shell.tabSuccessivi")}
            style={frecciaStyle(scorribile.destra)}
          >
            ›
          </button>
        </div>
      )}

      {/* La visibilita' del pannello inferiore la governano i pulsanti di layout
          nell'header dell'IDE (punto unico, regola L): qui non c'e' un secondo
          comando di chiusura. Un ✕ in fondo alle linguette suggeriva anche la cosa
          sbagliata — sembrava chiudere la linguetta attiva, non il pannello. */}
      {activeProject && (
        <div style={{ display: "flex", alignItems: "center", paddingRight: 12, paddingLeft: 8, flexShrink: 0 }}>
          <QuotaBadge projectId={activeProject.id} />
        </div>
      )}
    </div>
  );
}
