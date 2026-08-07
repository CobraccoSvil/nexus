"use client";

import type { GitHubAccountStatus } from "../../../lib/api-client";
import { useThemeColors } from "../../../lib/theme";
import { cardStyle, smallButtonStyle, statusBadgeStyle } from "./styles";
import { accountLabel, accountTone } from "./labels";
import { useI18n } from "../../../lib/i18n";

interface GitHubAccountCardProps {
  githubAccount: GitHubAccountStatus | null;
  busy: boolean;
  githubBusy: boolean;
  connectLabel: string;
  onConnect: () => void;
  onDisconnect: () => void;
}

export function GitHubAccountCard({
  githubAccount,
  busy,
  githubBusy,
  connectLabel,
  onConnect,
  onDisconnect,
}: GitHubAccountCardProps) {
  const { t } = useI18n();
  const tc = useThemeColors();

  return (
    <div style={cardStyle(tc)}>
      <div style={{ display: "flex", justifyContent: "space-between", gap: 12, flexWrap: "wrap" }}>
        <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          <div style={{ color: tc.text, fontWeight: 700 }}>GitHub</div>
          <div style={{ color: tc.textSecondary }}>{accountLabel(githubAccount)}</div>
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
          <span style={statusBadgeStyle(tc, accountTone(githubAccount?.status))}>
            {githubAccount?.status ?? "loading"}
          </span>
          {githubAccount?.connected ? (
            <button
              disabled={busy || githubBusy}
              onClick={() => void onDisconnect()}
              style={smallButtonStyle(tc, busy || githubBusy)}
            >
              {t("git.scollega")}
            </button>
          ) : (
            <button
              disabled={busy || githubBusy}
              onClick={() => void onConnect()}
              style={smallButtonStyle(tc, busy || githubBusy)}
            >
              {connectLabel}
            </button>
          )}
        </div>
      </div>
      {githubAccount?.expiresAt ? (
        <div style={{ color: tc.textMuted, fontSize: 11 }}>
          Token valido fino a {new Date(githubAccount.expiresAt).toLocaleString()}
        </div>
      ) : null}
    </div>
  );
}
