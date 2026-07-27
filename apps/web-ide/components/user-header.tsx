"use client";

import { useCallback, useEffect, useState, useRef } from "react";
import { usePathname } from "next/navigation";
import { useDismissOnOutside } from "../hooks/use-dismiss-on-outside";
import { fetchMe, logout, User } from "../lib/auth";
import { useThemeColors } from "../lib/theme";
import { useI18n } from "../lib/i18n";

type ThemeColors = ReturnType<typeof useThemeColors>;

/**
 * Variante inline per header orizzontali (usata nell'admin layout).
 */
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
          Admin
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
        Esci
      </button>
    </div>
  );
}

/**
 * Menu utente per la activity bar laterale dell'IDE.
 * Mostra l'avatar in basso; al clic apre un popup con nome, link admin e logout.
 */
export function UserSidebarMenu({
  buttonSize,
  tc,
}: {
  buttonSize: number;
  tc: ThemeColors;
}) {
  const [user, setUser] = useState<User | null>(null);
  const [open, setOpen] = useState(false);
  const { t } = useI18n();
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    fetchMe().then(setUser);
  }, []);

  // Chiudi il menu al clic fuori. Delegando al punto unico il menu guadagna anche
  // la chiusura con Escape, che prima non aveva: era l'unico popover dell'IDE a
  // restare aperto sotto Escape.
  const chiudiMenu = useCallback(() => setOpen(false), []);
  useDismissOnOutside(open, menuRef, chiudiMenu);

  if (!user) return null;

  const initials = (user.display_name || user.github_username || "U")
    .split(/\s+/)
    .map((w) => w[0])
    .join("")
    .slice(0, 2)
    .toUpperCase();

  return (
    <div ref={menuRef} style={{ position: "relative" }}>
      {/* Pulsante avatar */}
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        title={user.github_username || user.display_name}
        aria-label={user.github_username || user.display_name}
        style={{
          width: buttonSize,
          height: buttonSize,
          borderRadius: "50%",
          border: `1px solid ${open ? tc.accent : tc.border}`,
          background: open ? tc.accentBg : "transparent",
          cursor: "pointer",
          padding: 0,
          overflow: "hidden",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        {user.avatar_url ? (
          <img
            src={user.avatar_url}
            alt=""
            width={buttonSize - 2}
            height={buttonSize - 2}
            style={{ borderRadius: "50%", display: "block" }}
          />
        ) : (
          <span style={{
            fontSize: 11,
            fontWeight: 700,
            color: tc.text,
            letterSpacing: "0.04em",
          }}>
            {initials}
          </span>
        )}
      </button>

      {/* Popup menu */}
      {open && (
        <div
          style={{
            position: "absolute",
            bottom: 0,
            left: buttonSize + 8,
            minWidth: 200,
            background: tc.bgCard,
            border: `1px solid ${tc.border}`,
            borderRadius: 10,
            boxShadow: "0 8px 24px rgba(0,0,0,0.22)",
            zIndex: 1000,
            padding: "8px 0",
            display: "flex",
            flexDirection: "column",
          }}
        >
          {/* Info utente */}
          <div style={{
            padding: "8px 14px 10px",
            borderBottom: `1px solid ${tc.border}`,
            display: "flex",
            alignItems: "center",
            gap: 10,
          }}>
            {user.avatar_url && (
              <img
                src={user.avatar_url}
                alt=""
                width={32}
                height={32}
                style={{ borderRadius: "50%", border: `1px solid ${tc.border}` }}
              />
            )}
            <div>
              <div style={{ fontSize: 13, fontWeight: 700, color: tc.text }}>
                {user.display_name || user.github_username}
              </div>
              {user.github_username && user.display_name && (
                <div style={{ fontSize: 11, color: tc.textMuted }}>
                  @{user.github_username}
                </div>
              )}
              {user.role === "admin" && (
                <span style={{
                  fontSize: 9,
                  fontWeight: 700,
                  color: tc.accent,
                  background: tc.accentBg,
                  borderRadius: 4,
                  padding: "1px 5px",
                  textTransform: "uppercase",
                  letterSpacing: "0.06em",
                }}>
                  Admin
                </span>
              )}
            </div>
          </div>

          {/* Voci menu */}
          {user.role === "admin" && (
            <MenuLink href="/admin" tc={tc} onClick={() => setOpen(false)}>
              Pannello Admin
            </MenuLink>
          )}
          <MenuLink href="/?site" tc={tc} onClick={() => setOpen(false)}>
            {t("admin.link.site")}
          </MenuLink>

          <div style={{ height: 1, background: tc.border, margin: "4px 0" }} />

          <button
            type="button"
            onClick={() => { setOpen(false); logout(); }}
            style={{
              display: "flex",
              alignItems: "center",
              gap: 8,
              padding: "8px 14px",
              background: "none",
              border: "none",
              cursor: "pointer",
              color: tc.error,
              fontSize: 12,
              fontWeight: 600,
              width: "100%",
              textAlign: "left",
            }}
            onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.background = `${tc.error}10`; }}
            onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.background = "none"; }}
          >
            {t("auth.logout")}
          </button>
        </div>
      )}
    </div>
  );
}

function MenuLink({
  href,
  tc,
  onClick,
  children,
}: {
  href: string;
  tc: ThemeColors;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <a
      href={href}
      onClick={onClick}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 8,
        padding: "8px 14px",
        color: tc.text,
        fontSize: 12,
        fontWeight: 500,
        textDecoration: "none",
        transition: "background 0.12s",
      }}
      onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.background = `${tc.border}30`; }}
      onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.background = "none"; }}
    >
      {children}
    </a>
  );
}
