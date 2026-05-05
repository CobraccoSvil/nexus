"use client";

import { useEffect, useState, type ReactNode } from "react";
import { ThemeProvider, useThemeColors } from "../lib/theme";
import { I18nProvider } from "../lib/i18n";
import { GlobalDialogProvider } from "../components/global-dialog-provider";
import { GlobalActionFeedbackProvider } from "../components/global-action-feedback-provider";
import { PendingOperationsProvider } from "../lib/pending-operations-context";

function ThemedBody({ children }: { children: ReactNode }) {
  const t = useThemeColors();
  const [mounted, setMounted] = useState(false);

  useEffect(() => {
    setMounted(true);
  }, []);

  const cssVariables = `
    :root {
      --color-bg: ${t.bg};
      --color-bgCard: ${t.bgCard};
      --color-bgInput: ${t.bgInput};
      --color-bgHover: ${t.bgHover};
      --color-bgActive: ${t.bgActive};
      --color-bgHeader: ${t.bgHeader};
      --color-bgSidebar: ${t.bgSidebar};
      --color-border: ${t.border};
      --color-text: ${t.text};
      --color-textSecondary: ${t.textSecondary};
      --color-textMuted: ${t.textMuted};
      --color-accent: ${t.accent};
      --color-accentBg: ${t.accentBg};
      --color-success: ${t.success};
      --color-error: ${t.error};
      --color-warning: ${t.warning};
    }
  `;

  return (
    <>
      <style>{cssVariables}</style>
      <div
        style={{
          background: mounted ? t.bg : "transparent",
          color: mounted ? t.text : "transparent",
          fontFamily: "JetBrains Mono, Fira Code, monospace",
          visibility: mounted ? "visible" : "hidden",
          minHeight: "100vh",
        }}
      >
        {children}
      </div>
    </>
  );
}

export function ThemeBody({ children }: { children: ReactNode }) {
  return (
    <PendingOperationsProvider>
      <ThemeProvider>
        <I18nProvider>
          <GlobalDialogProvider>
            <GlobalActionFeedbackProvider>
              <ThemedBody>{children}</ThemedBody>
            </GlobalActionFeedbackProvider>
          </GlobalDialogProvider>
        </I18nProvider>
      </ThemeProvider>
    </PendingOperationsProvider>
  );
}