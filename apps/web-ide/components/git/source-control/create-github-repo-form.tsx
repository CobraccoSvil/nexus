"use client";

import type { Theme } from "../../../lib/theme";
import { inputStyle, smallButtonStyle } from "./styles";
import { useI18n } from "../../../lib/i18n";

interface Props {
  tc: Theme;
  /** Titolo opzionale sopra la form (es. "Crea repository GitHub"). */
  title?: string;
  createRepoName: string;
  setCreateRepoName: (v: string) => void;
  createRepoDesc: string;
  setCreateRepoDesc: (v: string) => void;
  createRepoPrivate: boolean;
  setCreateRepoPrivate: (v: boolean) => void;
  createRepoBusy: boolean;
  /** Label del bottone conferma a stati busy/idle (es. ["Conferma e pubblica", "Pubblicazione in corso..."]). */
  confirmLabels: { idle: string; busy: string };
  /** Hint di processo opzionale sotto i bottoni. */
  hint?: string;
  onConfirm: () => void;
  onCancel: () => void;
}

/** Form "Crea repository GitHub" condivisa tra github-import-section e remote-card
 *  (regola L / ADR 0026). Prima il blocco input-nome + input-desc + checkbox
 *  privato + 2 bottoni era duplicato cross-file (38L cluster jscpd). */
export function CreateGithubRepoForm({
  tc,
  title,
  createRepoName,
  setCreateRepoName,
  createRepoDesc,
  setCreateRepoDesc,
  createRepoPrivate,
  setCreateRepoPrivate,
  createRepoBusy,
  confirmLabels,
  hint,
  onConfirm,
  onCancel,
}: Props) {
  const { t } = useI18n();
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      {title ? <div style={{ color: tc.text, fontWeight: 700 }}>{title}</div> : null}
      <input
        value={createRepoName}
        onChange={(e) => setCreateRepoName(e.target.value)}
        placeholder={t("git.nomeRepositoryAlfanumerico")}
        style={inputStyle(tc)}
      />
      <input
        value={createRepoDesc}
        onChange={(e) => setCreateRepoDesc(e.target.value)}
        placeholder={t("git.descrizioneOpzionale")}
        style={inputStyle(tc)}
      />
      <label style={{ display: "flex", alignItems: "center", gap: 8, color: tc.text }}>
        <input
          type="checkbox"
          checked={createRepoPrivate}
          onChange={(e) => setCreateRepoPrivate(e.target.checked)}
        />
        {t("git.repositoryPrivato")}
      </label>
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
        <button
          disabled={createRepoBusy || !createRepoName.trim()}
          onClick={() => void onConfirm()}
          style={smallButtonStyle(tc, createRepoBusy || !createRepoName.trim())}
        >
          {createRepoBusy ? confirmLabels.busy : confirmLabels.idle}
        </button>
        <button
          disabled={createRepoBusy}
          onClick={onCancel}
          style={smallButtonStyle(tc, createRepoBusy)}
        >
          {t("git.annulla")}
        </button>
      </div>
      {hint ? (
        <div style={{ fontSize: 10, color: tc.textMuted }}>{hint}</div>
      ) : null}
    </div>
  );
}
