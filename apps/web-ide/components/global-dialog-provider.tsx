"use client";

import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from "react";
import { useThemeColors } from "../lib/theme";

type DialogKind = "alert" | "confirm" | "prompt";

type DialogRequest = {
  kind: DialogKind;
  title?: string;
  message: string;
  defaultValue?: string;
};

type DialogState = DialogRequest & {
  resolve: (value: string | boolean | null | void) => void;
};

type DialogApi = {
  alertDialog: (message: string, title?: string) => Promise<void>;
  confirmDialog: (message: string, title?: string) => Promise<boolean>;
  promptDialog: (message: string, defaultValue?: string, title?: string) => Promise<string | null>;
};

const DialogContext = createContext<DialogApi | null>(null);

export function GlobalDialogProvider({ children }: { children: ReactNode }) {
  const tc = useThemeColors();
  const [dialog, setDialog] = useState<DialogState | null>(null);
  const [promptValue, setPromptValue] = useState("");

  const openDialog = useCallback(
    <T,>(request: DialogRequest) =>
      new Promise<T>((resolve) => {
        setPromptValue(request.defaultValue ?? "");
        setDialog({
          ...request,
          resolve: (value) => resolve(value as T),
        });
      }),
    [],
  );

  const alertDialog = useCallback(
    async (message: string, title?: string) => {
      await openDialog<void>({ kind: "alert", message, title });
    },
    [openDialog],
  );

  const confirmDialog = useCallback(
    (message: string, title?: string) =>
      openDialog<boolean>({ kind: "confirm", message, title }),
    [openDialog],
  );

  const promptDialog = useCallback(
    (message: string, defaultValue?: string, title?: string) =>
      openDialog<string | null>({ kind: "prompt", message, defaultValue, title }),
    [openDialog],
  );

  const close = useCallback(
    (value: string | boolean | null | void) => {
      if (!dialog) return;
      dialog.resolve(value);
      setDialog(null);
    },
    [dialog],
  );

  const api = useMemo<DialogApi>(
    () => ({ alertDialog, confirmDialog, promptDialog }),
    [alertDialog, confirmDialog, promptDialog],
  );

  return (
    <DialogContext.Provider value={api}>
      {children}
      {dialog && (
        <div
          style={{
            position: "fixed",
            inset: 0,
            background: "rgba(5, 10, 18, 0.46)",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            zIndex: 1200,
            padding: 16,
          }}
          onClick={() => close(dialog.kind === "confirm" ? false : dialog.kind === "prompt" ? null : undefined)}
        >
          <div
            role="dialog"
            aria-modal="true"
            style={{
              width: 480,
              maxWidth: "95vw",
              borderRadius: 10,
              border: `1px solid ${tc.border}`,
              background: tc.bgCard,
              boxShadow: "0 14px 44px rgba(0,0,0,0.35)",
              padding: 14,
              display: "flex",
              flexDirection: "column",
              gap: 10,
            }}
            onClick={(event) => event.stopPropagation()}
          >
            <div style={{ color: tc.text, fontWeight: 700, fontSize: 14 }}>
              {dialog.title ?? "Conferma"}
            </div>
            <div style={{ color: tc.textSecondary, fontSize: 13, whiteSpace: "pre-wrap" }}>
              {dialog.message}
            </div>
            {dialog.kind === "prompt" && (
              <input
                autoFocus
                value={promptValue}
                onChange={(event) => setPromptValue(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") close(promptValue);
                  if (event.key === "Escape") close(null);
                }}
                style={{
                  width: "100%",
                  borderRadius: 8,
                  border: `1px solid ${tc.border}`,
                  background: tc.bgInput,
                  color: tc.text,
                  padding: "8px 10px",
                  fontSize: 13,
                  boxSizing: "border-box",
                }}
              />
            )}
            <div style={{ display: "flex", justifyContent: "flex-end", gap: 8 }}>
              {dialog.kind !== "alert" && (
                <button
                  type="button"
                  onClick={() =>
                    close(dialog.kind === "confirm" ? false : null)
                  }
                  style={dialogButton(tc)}
                >
                  Annulla
                </button>
              )}
              <button
                type="button"
                autoFocus={dialog.kind !== "prompt"}
                onClick={() => close(dialog.kind === "prompt" ? promptValue : true)}
                style={dialogButton(tc, true)}
              >
                OK
              </button>
            </div>
          </div>
        </div>
      )}
    </DialogContext.Provider>
  );
}

export function useGlobalDialog() {
  const ctx = useContext(DialogContext);
  if (!ctx) {
    throw new Error("useGlobalDialog must be used inside GlobalDialogProvider");
  }
  return ctx;
}

function dialogButton(tc: ReturnType<typeof useThemeColors>, primary = false) {
  return {
    border: `1px solid ${primary ? tc.accent : tc.border}`,
    background: primary ? tc.accentBg : tc.bgInput,
    color: primary ? tc.accent : tc.text,
    borderRadius: 8,
    padding: "6px 12px",
    fontSize: 12,
    cursor: "pointer",
    fontFamily: "inherit",
  } as const;
}
