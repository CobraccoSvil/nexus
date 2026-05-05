"use client";

import { createContext, useContext, useEffect, useState, type ReactNode } from "react";

export type ThemeMode = "light" | "dark" | "auto";

interface ThemeContextValue {
  mode: ThemeMode;
  resolved: "light" | "dark";
  setMode: (m: ThemeMode) => void;
}

const ThemeContext = createContext<ThemeContextValue>({
  mode: "dark",
  resolved: "dark",
  setMode: () => {},
});

export function useTheme() {
  return useContext(ThemeContext);
}

function getSystemPreference(): "light" | "dark" {
  if (typeof window === "undefined") return "dark";
  return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

export const themes = {
  dark: {
    bg: "#08111d",
    bgGradient: "linear-gradient(180deg, #08111d 0%, #0b1524 100%)",
    bgCard: "#0f1723",
    bgInput: "#0a1520",
    bgHeader: "rgba(8, 17, 29, 0.95)",
    bgSidebar: "#0a1320",
    bgHover: "#162236",
    bgActive: "#163a63",
    border: "#22324a",
    text: "#edf2f7",
    textSecondary: "#8ba3c1",
    textMuted: "#6b7f99",
    accent: "#5ba3e6",
    accentBg: "#163a63",
    success: "#4ade80",
    error: "#f87171",
    warning: "#fbbf24",
  },
  light: {
    bg: "#f5f7fa",
    bgGradient: "linear-gradient(180deg, #f5f7fa 0%, #e8ecf1 100%)",
    bgCard: "#ffffff",
    bgInput: "#f0f2f5",
    bgHeader: "rgba(255, 255, 255, 0.95)",
    bgSidebar: "#ffffff",
    bgHover: "#e8ecf1",
    bgActive: "#d0e2f7",
    border: "#d1d9e6",
    text: "#1a2332",
    textSecondary: "#4a5568",
    textMuted: "#718096",
    accent: "#2b6cb0",
    accentBg: "#d0e2f7",
    success: "#22c55e",
    error: "#ef4444",
    warning: "#f59e0b",
  },
} as const;

export type Theme = { [K in keyof typeof themes.dark]: string };

export function useThemeColors(): Theme {
  const { resolved } = useTheme();
  return themes[resolved];
}

function getSavedMode(): ThemeMode {
  if (typeof window === "undefined") return "dark";
  const saved = localStorage.getItem("nexus-theme");
  if (saved && ["light", "dark", "auto"].includes(saved)) return saved as ThemeMode;
  return "dark";
}

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [mode, setModeState] = useState<ThemeMode>("dark");
  const [systemPref, setSystemPref] = useState<"light" | "dark">("dark");

  useEffect(() => {
    setModeState(getSavedMode());
    setSystemPref(getSystemPreference());

    const mq = window.matchMedia("(prefers-color-scheme: light)");
    const handler = (e: MediaQueryListEvent) => setSystemPref(e.matches ? "light" : "dark");
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, []);

  const setMode = (m: ThemeMode) => {
    setModeState(m);
    localStorage.setItem("nexus-theme", m);
  };

  const resolved = mode === "auto" ? systemPref : mode;

  return (
    <ThemeContext.Provider value={{ mode, resolved, setMode }}>
      {children}
    </ThemeContext.Provider>
  );
}
