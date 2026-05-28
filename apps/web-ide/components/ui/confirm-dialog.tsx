"use client";

import { Fragment, useEffect, useRef, useState } from "react";
import { useThemeColors } from "../../lib/theme";

/**
 * Dialog modale inline riutilizzabile per conferme, prompt e alert.
 *
 * Estratto dal pattern usato in `project-explorer.tsx::ExplorerModal` (fix
 * task #57) per sostituire le `window.confirm/prompt/alert` native che:
 *  - bloccano i driver di automazione (Chrome MCP / Playwright)
 *  - hanno UX inconsistente col resto della UI
 *  - non consentono titoli, stili danger, label di azione personalizzate
 *
 * API Promise-based: `useConfirmDialog()` ritorna `{ confirm, prompt, alert,
 * dialogElement }`. Renderizza `dialogElement` una sola volta in fondo al
 * componente; ogni chiamata await produce una Promise risolta con la scelta
 * dell'utente.
 */

type DialogState =
  | { kind: "alert"; title: string; message: string; resolve: (v: void) => void }
  | {
      kind: "confirm";
      title: string;
      message: string;
      danger?: boolean;
      confirmLabel?: string;
      cancelLabel?: string;
      resolve: (v: boolean) => void;
    }
  | {
      kind: "prompt";
      title: string;
      label: string;
      defaultValue?: string;
      placeholder?: string;
      resolve: (v: string | null) => void;
    }
  | null;

export interface ConfirmOptions {
  title: string;
  message: string;
  danger?: boolean;
  confirmLabel?: string;
  cancelLabel?: string;
}
export interface PromptOptions {
  title: string;
  label: string;
  defaultValue?: string;
  placeholder?: string;
}
export interface AlertOptions {
  title: string;
  message: string;
}

export function useConfirmDialog(): {
  confirm: (opts: ConfirmOptions) => Promise<boolean>;
  prompt: (opts: PromptOptions) => Promise<string | null>;
  alert: (opts: AlertOptions) => Promise<void>;
  dialogElement: React.ReactNode;
} {
  const [state, setState] = useState<DialogState>(null);

  const confirm = (opts: ConfirmOptions) =>
    new Promise<boolean>((resolve) => setState({ kind: "confirm", resolve, ...opts }));
  const prompt = (opts: PromptOptions) =>
    new Promise<string | null>((resolve) => setState({ kind: "prompt", resolve, ...opts }));
  const alert = (opts: AlertOptions) =>
    new Promise<void>((resolve) => setState({ kind: "alert", resolve, ...opts }));

  const dialogElement = state ? (
    <ConfirmDialog
      state={state}
      onResolveAlert={() => {
        if (state.kind === "alert") state.resolve();
        setState(null);
      }}
      onResolveConfirm={(v) => {
        if (state.kind === "confirm") state.resolve(v);
        setState(null);
      }}
      onResolvePrompt={(v) => {
        if (state.kind === "prompt") state.resolve(v);
        setState(null);
      }}
    />
  ) : null;

  return { confirm, prompt, alert, dialogElement };
}

function ConfirmDialog({
  state,
  onResolveAlert,
  onResolveConfirm,
  onResolvePrompt,
}: {
  state: NonNullable<DialogState>;
  onResolveAlert: () => void;
  onResolveConfirm: (v: boolean) => void;
  onResolvePrompt: (v: string | null) => void;
}) {
  const tc = useThemeColors();
  const inputRef = useRef<HTMLInputElement>(null);
  const okButtonRef = useRef<HTMLButtonElement>(null);
  const [inputValue, setInputValue] = useState<string>(
    state.kind === "prompt" ? state.defaultValue ?? "" : "",
  );

  useEffect(() => {
    const t = window.setTimeout(() => {
      if (state.kind === "prompt") {
        inputRef.current?.focus();
        inputRef.current?.select();
      } else {
        okButtonRef.current?.focus();
      }
    }, 30);
    return () => window.clearTimeout(t);
  }, [state.kind]);

  useEffect(() => {
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key !== "Escape") return;
      if (state.kind === "alert") onResolveAlert();
      else if (state.kind === "confirm") onResolveConfirm(false);
      else if (state.kind === "prompt") onResolvePrompt(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [state.kind, onResolveAlert, onResolveConfirm, onResolvePrompt]);

  const handleOk = () => {
    if (state.kind === "alert") onResolveAlert();
    else if (state.kind === "confirm") onResolveConfirm(true);
    else if (state.kind === "prompt")
      onResolvePrompt(inputValue.trim() === "" ? null : inputValue);
  };
  const handleCancel = () => {
    if (state.kind === "confirm") onResolveConfirm(false);
    else if (state.kind === "prompt") onResolvePrompt(null);
    else onResolveAlert();
  };

  const isDanger = state.kind === "confirm" && state.danger === true;
  const confirmLabel =
    state.kind === "confirm"
      ? state.confirmLabel ?? "OK"
      : state.kind === "prompt"
        ? "Conferma"
        : "OK";
  const cancelLabel = state.kind === "confirm" ? state.cancelLabel ?? "Annulla" : "Annulla";

  return (
    <div
      role="presentation"
      onMouseDown={(ev) => {
        if (ev.target === ev.currentTarget) handleCancel();
      }}
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 10000,
        background: "rgba(0,0,0,0.55)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: 16,
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="confirm-dialog-title"
        onKeyDown={(ev) => {
          if (ev.key === "Enter" && state.kind !== "alert") {
            if (state.kind === "prompt" && ev.target instanceof HTMLInputElement) {
              ev.preventDefault();
              handleOk();
            } else if (state.kind === "confirm") {
              ev.preventDefault();
              handleOk();
            }
          }
        }}
        style={{
          minWidth: 360,
          maxWidth: 520,
          background: tc.bg ?? "#1f1f1f",
          color: tc.text,
          border: `1px solid ${tc.border}`,
          borderRadius: 10,
          boxShadow: "0 16px 48px rgba(0,0,0,0.5)",
          padding: 20,
          display: "flex",
          flexDirection: "column",
          gap: 14,
        }}
      >
        <div id="confirm-dialog-title" style={{ fontSize: 15, fontWeight: 600, color: tc.text }}>
          {state.title}
        </div>

        {state.kind !== "prompt" && (
          <div style={{ whiteSpace: "pre-wrap", fontSize: 13, color: tc.textMuted }}>
            {state.message}
          </div>
        )}
        {state.kind === "prompt" && (
          <label style={{ display: "flex", flexDirection: "column", gap: 6 }}>
            <span style={{ fontSize: 12, color: tc.textMuted }}>{state.label}</span>
            <input
              ref={inputRef}
              type="text"
              value={inputValue}
              onChange={(ev) => setInputValue(ev.target.value)}
              placeholder={state.placeholder}
              style={{
                padding: "8px 10px",
                fontSize: 13,
                borderRadius: 6,
                border: `1px solid ${tc.border}`,
                background: tc.bg ?? "#0f0f0f",
                color: tc.text,
                outline: "none",
              }}
            />
          </label>
        )}

        <div
          style={{
            display: "flex",
            justifyContent: "flex-end",
            gap: 8,
            marginTop: 4,
          }}
        >
          {state.kind !== "alert" && (
            <button
              type="button"
              onClick={handleCancel}
              style={{
                padding: "8px 14px",
                fontSize: 13,
                borderRadius: 6,
                border: `1px solid ${tc.border}`,
                background: "transparent",
                color: tc.text,
                cursor: "pointer",
              }}
            >
              {cancelLabel}
            </button>
          )}
          <button
            ref={okButtonRef}
            type="button"
            onClick={handleOk}
            data-testid="confirm-dialog-ok"
            style={{
              padding: "8px 14px",
              fontSize: 13,
              borderRadius: 6,
              border: "none",
              background: isDanger ? "#dc2626" : (tc.accent ?? "#2563eb"),
              color: "#ffffff",
              cursor: "pointer",
              fontWeight: 600,
            }}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

// Fragment per evitare warning di import non usato in build standalone.
export const _ConfirmDialogFragment = Fragment;
