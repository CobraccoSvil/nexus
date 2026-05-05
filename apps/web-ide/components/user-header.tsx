"use client";

import { useEffect, useState } from "react";
import { usePathname } from "next/navigation";
import { fetchMe, logout, User } from "../lib/auth";
import { useThemeColors } from "../lib/theme";
import { useI18n } from "../lib/i18n";

export function UserHeader() {
  const [user, setUser] = useState<User | null>(null);
  const tc = useThemeColors();
  const { t } = useI18n();
  const pathname = usePathname();
  const isAdminArea = pathname?.startsWith("/admin");

  useEffect(() => {
    fetchMe().then(setUser);
  }, []);

  if (!user) return null;

  return (
    <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
      {user.avatar_url && (
        <img
          src={user.avatar_url}
          alt=""
          width={24}
          height={24}
          style={{ borderRadius: "50%", border: `1px solid ${tc.border}` }}
        />
      )}
      <span style={{ fontSize: 12, color: tc.textSecondary }}>
        {user.github_username || user.display_name}
      </span>
      {user.role === "admin" && !isAdminArea && (
        <a
          href="/admin"
          title="Apri area Admin"
          aria-label="Apri area Admin"
          style={{
            width: 26,
            height: 26,
            display: "inline-flex",
            alignItems: "center",
            justifyContent: "center",
            borderRadius: 5,
            background: tc.accentBg,
            color: tc.text,
            fontSize: 13,
            textDecoration: "none",
            fontWeight: 600,
          }}
        >
          🛠
        </a>
      )}
      {user.role === "admin" && isAdminArea && (
        <a
          href="/"
          title="Apri IDE"
          aria-label="Apri IDE"
          style={{
            height: 26,
            display: "inline-flex",
            alignItems: "center",
            justifyContent: "center",
            borderRadius: 5,
            border: `1px solid ${tc.border}`,
            background: "transparent",
            color: tc.text,
            fontSize: 11,
            textDecoration: "none",
            padding: "0 8px",
            fontWeight: 700,
            letterSpacing: "0.04em",
          }}
        >
          IDE
        </a>
      )}
      <button
        onClick={logout}
        title={t("auth.logout")}
        aria-label={t("auth.logout")}
        style={{
          width: 26,
          height: 26,
          display: "inline-flex",
          alignItems: "center",
          justifyContent: "center",
          borderRadius: 5,
          border: `1px solid ${tc.border}`,
          background: "transparent",
          color: tc.textMuted,
          fontSize: 13,
          cursor: "pointer",
        }}
      >
        ⎋
      </button>
    </div>
  );
}
