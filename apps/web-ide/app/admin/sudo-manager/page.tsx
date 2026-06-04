"use client";

/**
 * Admin / Sudo Manager (ADR 0017 Livello 1)
 *
 * Gestione della whitelist purposes che possono essere eseguiti via
 * /usr/local/bin/nexus-sudo-runner con NOPASSWD. Mai password sudo
 * salvate. Solo comandi noti, validati lato runner via PATH_ALLOWLIST.
 */

import { useCallback, useEffect, useState } from "react";
import {
  createSudoPurpose,
  deleteSudoPurpose,
  executeSudoPurpose,
  getSudoManagerStatus,
  listSudoAudit,
  listSudoPurposes,
  patchSudoPurpose,
  type SudoAuditEntry,
  type SudoManagerStatus,
  type SudoPurpose,
} from "../../../lib/api-client";
import { useThemeColors } from "../../../lib/theme";
import { useGlobalDialog } from "../../../components/global-dialog-provider";

export default function SudoManagerAdminPage() {
  const tc = useThemeColors();
  const dialog = useGlobalDialog();
  const [status, setStatus] = useState<SudoManagerStatus | null>(null);
  const [purposes, setPurposes] = useState<SudoPurpose[]>([]);
  const [audit, setAudit] = useState<SudoAuditEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [executing, setExecuting] = useState<string | null>(null);
  const [lastResult, setLastResult] = useState<{
    purpose: string;
    exit_code: number;
    stdout: string;
    stderr: string;
  } | null>(null);
  const [showCreate, setShowCreate] = useState(false);

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [s, p, a] = await Promise.all([
        getSudoManagerStatus(),
        listSudoPurposes(),
        listSudoAudit({ limit: 30 }),
      ]);
      setStatus(s);
      setPurposes(p.items);
      setAudit(a.items);
    } catch (e: unknown) {
      setError(String((e as Error)?.message ?? e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const onExecute = async (p: SudoPurpose) => {
    if (p.requires_confirm) {
      const ok = await dialog.confirmDialog(
        `Eseguire purpose "${p.name}"?\n\nComando:\n${p.command_template}\n\nL'azione viene loggata nell'audit log.`,
      );
      if (!ok) return;
    }
    setExecuting(p.id);
    setError(null);
    try {
      const r = await executeSudoPurpose(p.name);
      setLastResult({
        purpose: r.purpose,
        exit_code: r.exit_code,
        stdout: r.stdout,
        stderr: r.stderr,
      });
      await reload();
    } catch (e: unknown) {
      setError(String((e as Error)?.message ?? e));
    } finally {
      setExecuting(null);
    }
  };

  const onToggleEnabled = async (p: SudoPurpose) => {
    try {
      await patchSudoPurpose(p.id, { enabled: !p.enabled });
      await reload();
    } catch (e: unknown) {
      setError(String((e as Error)?.message ?? e));
    }
  };

  const onDelete = async (p: SudoPurpose) => {
    const ok = await dialog.confirmDialog(
      `Rimuovere il purpose "${p.name}"?\nVerra' cancellato dalla whitelist.`,
    );
    if (!ok) return;
    try {
      await deleteSudoPurpose(p.id);
      await reload();
    } catch (e: unknown) {
      setError(String((e as Error)?.message ?? e));
    }
  };

  const categories = Array.from(new Set(purposes.map((p) => p.category))).sort();
  const setupReady =
    status?.enabled && status?.runner_installed && status?.sudoers_installed;

  return (
    <div style={{ padding: 16, color: tc.text }}>
      <header style={{ marginBottom: 14 }}>
        <h1 style={{ fontSize: 22, fontWeight: 700, margin: "0 0 4px" }}>
          Sudo Manager
        </h1>
        <p style={{ fontSize: 12, color: tc.textMuted, margin: 0 }}>
          Whitelist di comandi privilegiati eseguibili da Nexus. Niente password
          sudo salvate: la sicurezza viene da <code>/etc/sudoers.d/nexus-runner</code>.
          Setup one-time: <code>bash deploy/install-sudo-manager.sh</code>.
        </p>
      </header>

      {/* Status banner */}
      <section
        style={{
          padding: 12,
          borderRadius: 8,
          border: `1px solid ${setupReady ? tc.success : tc.warning}`,
          background: tc.bgCard,
          marginBottom: 16,
        }}
      >
        <div style={{ fontWeight: 700, marginBottom: 6 }}>Stato installazione</div>
        {!status ? (
          <div style={{ color: tc.textMuted }}>Caricamento…</div>
        ) : (
          <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fit, minmax(220px, 1fr))", gap: 8, fontSize: 12 }}>
            <StatusItem label="Enabled" ok={status.enabled} value={status.enabled ? "yes" : "no"} />
            <StatusItem label="Runner installato" ok={status.runner_installed} value={status.runner_installed ? status.runner_path : "MANCA"} />
            <StatusItem label="sudoers.d installato" ok={status.sudoers_installed} value={status.sudoers_installed ? "/etc/sudoers.d/nexus-runner" : "MANCA"} />
            <StatusItem label="Purposes attivi" ok={status.purposes_count > 0} value={String(status.purposes_count)} />
            <StatusItem label="Esecuzioni 24h" ok={true} value={String(status.audit_recent_count)} />
          </div>
        )}
        {!setupReady && status && (
          <div style={{ marginTop: 10, padding: 8, background: tc.bgInput, borderRadius: 4, fontSize: 12 }}>
            Esegui setup one-time:
            <code style={{ display: "block", marginTop: 4, color: tc.accent }}>
              bash deploy/install-sudo-manager.sh
            </code>
          </div>
        )}
      </section>

      {/* Errori */}
      {error && (
        <div
          style={{
            padding: 10,
            borderRadius: 6,
            border: `1px solid ${tc.error}`,
            background: tc.bgCard,
            color: tc.error,
            marginBottom: 12,
            fontSize: 12,
          }}
        >
          {error}
          <button
            onClick={() => setError(null)}
            style={{
              marginLeft: 12,
              background: "none",
              border: "none",
              color: tc.accent,
              cursor: "pointer",
              fontSize: 11,
            }}
          >
            chiudi
          </button>
        </div>
      )}

      {/* Header tabella + nuovo */}
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
        <h2 style={{ fontSize: 16, fontWeight: 600, margin: 0 }}>Purposes ({purposes.length})</h2>
        <div style={{ display: "flex", gap: 8 }}>
          <button
            type="button"
            onClick={() => void reload()}
            disabled={loading}
            style={btnStyle(tc, "secondary")}
          >
            {loading ? "..." : "Ricarica"}
          </button>
          <button
            type="button"
            onClick={() => setShowCreate(true)}
            style={btnStyle(tc, "primary")}
          >
            + Nuovo purpose
          </button>
        </div>
      </div>

      {/* Tabella purposes raggruppata per categoria */}
      {categories.map((cat) => (
        <section key={cat} style={{ marginBottom: 18 }}>
          <div style={{ fontSize: 12, color: tc.textMuted, fontWeight: 700, marginBottom: 6 }}>
            {cat.toUpperCase()}
          </div>
          <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 12.5 }}>
            <thead>
              <tr style={{ borderBottom: `1px solid ${tc.border}`, background: tc.bgCard }}>
                <th style={th()}>Nome</th>
                <th style={th()}>Descrizione</th>
                <th style={th()}>Comando</th>
                <th style={th()}>Enabled</th>
                <th style={th()}>Azioni</th>
              </tr>
            </thead>
            <tbody>
              {purposes
                .filter((p) => p.category === cat)
                .map((p) => (
                  <tr key={p.id} style={{ borderBottom: `1px solid ${tc.border}` }}>
                    <td style={td()}><code style={{ color: tc.accent }}>{p.name}</code></td>
                    <td style={{ ...td(), color: tc.textSecondary }}>{p.description}</td>
                    <td style={td()}>
                      <code style={{ fontSize: 11, color: tc.text }}>{p.command_template}</code>
                    </td>
                    <td style={td()}>
                      <button
                        type="button"
                        onClick={() => void onToggleEnabled(p)}
                        style={{
                          padding: "2px 8px",
                          background: p.enabled ? tc.success : tc.bgInput,
                          color: p.enabled ? "#fff" : tc.textMuted,
                          border: `1px solid ${p.enabled ? tc.success : tc.border}`,
                          borderRadius: 3,
                          cursor: "pointer",
                          fontSize: 11,
                        }}
                      >
                        {p.enabled ? "ON" : "OFF"}
                      </button>
                    </td>
                    <td style={td()}>
                      <button
                        type="button"
                        onClick={() => void onExecute(p)}
                        disabled={!p.enabled || executing === p.id || !setupReady}
                        style={{
                          ...btnStyle(tc, "primary"),
                          opacity: !p.enabled || !setupReady ? 0.5 : 1,
                          fontSize: 11,
                          padding: "3px 10px",
                          marginRight: 6,
                        }}
                      >
                        {executing === p.id ? "..." : "Esegui"}
                      </button>
                      <button
                        type="button"
                        onClick={() => void onDelete(p)}
                        style={{ ...btnStyle(tc, "secondary"), fontSize: 11, padding: "3px 10px" }}
                      >
                        Rimuovi
                      </button>
                    </td>
                  </tr>
                ))}
            </tbody>
          </table>
        </section>
      ))}

      {/* Last execution result */}
      {lastResult && (
        <section
          style={{
            padding: 12,
            borderRadius: 6,
            border: `1px solid ${lastResult.exit_code === 0 ? tc.success : tc.error}`,
            background: tc.bgCard,
            marginBottom: 16,
          }}
        >
          <div style={{ fontWeight: 700, marginBottom: 6 }}>
            Ultima esecuzione: <code>{lastResult.purpose}</code> (exit={lastResult.exit_code})
          </div>
          {lastResult.stdout && (
            <details open style={{ marginBottom: 6 }}>
              <summary style={{ cursor: "pointer", color: tc.textSecondary }}>stdout</summary>
              <pre style={preStyle(tc)}>{lastResult.stdout}</pre>
            </details>
          )}
          {lastResult.stderr && (
            <details style={{ marginBottom: 6 }}>
              <summary style={{ cursor: "pointer", color: tc.error }}>stderr</summary>
              <pre style={preStyle(tc)}>{lastResult.stderr}</pre>
            </details>
          )}
        </section>
      )}

      {/* Audit log */}
      <h2 style={{ fontSize: 16, fontWeight: 600, margin: "0 0 8px" }}>Audit log (ultimi 30)</h2>
      <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 12 }}>
        <thead>
          <tr style={{ borderBottom: `1px solid ${tc.border}`, background: tc.bgCard }}>
            <th style={th()}>Data</th>
            <th style={th()}>Purpose</th>
            <th style={th()}>Servizio</th>
            <th style={th()}>Exit</th>
            <th style={th()}>Durata</th>
            <th style={th()}>Comando</th>
          </tr>
        </thead>
        <tbody>
          {audit.length === 0 && (
            <tr><td colSpan={6} style={{ ...td(), color: tc.textMuted, fontStyle: "italic" }}>nessuna esecuzione</td></tr>
          )}
          {audit.map((a) => (
            <tr key={a.id} style={{ borderBottom: `1px solid ${tc.border}` }}>
              <td style={td()}>{a.executed_at ? new Date(a.executed_at).toLocaleString() : "—"}</td>
              <td style={td()}><code>{a.purpose_name}</code></td>
              <td style={{ ...td(), color: tc.textMuted }}>{a.requested_by_service ?? "—"}</td>
              <td style={{ ...td(), color: a.exit_code === 0 ? tc.success : tc.error }}>{a.exit_code ?? "?"}</td>
              <td style={{ ...td(), color: tc.textMuted }}>{a.duration_ms != null ? `${a.duration_ms} ms` : "—"}</td>
              <td style={td()}>
                <code style={{ fontSize: 11 }}>{a.full_command.length > 60 ? a.full_command.slice(0, 60) + "…" : a.full_command}</code>
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      {showCreate && (
        <CreatePurposeModal
          onClose={() => setShowCreate(false)}
          onCreated={async () => {
            setShowCreate(false);
            await reload();
          }}
          onError={setError}
        />
      )}
    </div>
  );
}

function CreatePurposeModal({
  onClose,
  onCreated,
  onError,
}: {
  onClose: () => void;
  onCreated: () => void | Promise<void>;
  onError: (msg: string) => void;
}) {
  const tc = useThemeColors();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [commandTemplate, setCommandTemplate] = useState("");
  const [category, setCategory] = useState("general");
  const [requiresConfirm, setRequiresConfirm] = useState(true);
  const [busy, setBusy] = useState(false);

  const onSubmit = async () => {
    setBusy(true);
    try {
      await createSudoPurpose({
        name: name.trim(),
        description: description.trim(),
        command_template: commandTemplate.trim(),
        category: category.trim() || "general",
        requires_confirm: requiresConfirm,
      });
      await onCreated();
    } catch (e: unknown) {
      onError(String((e as Error)?.message ?? e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      onClick={onClose}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,0.5)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 100,
      }}
    >
      <div
        onClick={(e) => e.stopPropagation()}
        style={{
          background: tc.bgCard,
          padding: 20,
          borderRadius: 8,
          border: `1px solid ${tc.border}`,
          width: 580,
          maxWidth: "90vw",
        }}
      >
        <h3 style={{ margin: "0 0 12px", fontSize: 16 }}>Nuovo purpose</h3>
        <FormField label="Nome (kebab-case, 3-64 char)">
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="es. install-postgres-client"
            style={inputStyle(tc)}
          />
        </FormField>
        <FormField label="Categoria">
          <input
            value={category}
            onChange={(e) => setCategory(e.target.value)}
            placeholder="general / playwright / service / filesystem"
            style={inputStyle(tc)}
          />
        </FormField>
        <FormField label="Descrizione">
          <textarea
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder="Cosa fa e quando usarlo"
            rows={2}
            style={{ ...inputStyle(tc), fontFamily: "inherit", resize: "vertical" }}
          />
        </FormField>
        <FormField label="Command template (no shell metachars)">
          <textarea
            value={commandTemplate}
            onChange={(e) => setCommandTemplate(e.target.value)}
            placeholder="apt-get install -y libfoo libbar"
            rows={2}
            style={{ ...inputStyle(tc), fontFamily: "monospace", resize: "vertical" }}
          />
        </FormField>
        <label style={{ display: "flex", gap: 6, fontSize: 12, color: tc.textMuted, marginTop: 8 }}>
          <input
            type="checkbox"
            checked={requiresConfirm}
            onChange={(e) => setRequiresConfirm(e.target.checked)}
          />
          Richiede conferma in UI prima dell'esecuzione
        </label>
        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end", marginTop: 14 }}>
          <button type="button" onClick={onClose} style={btnStyle(tc, "secondary")}>
            Annulla
          </button>
          <button
            type="button"
            onClick={() => void onSubmit()}
            disabled={busy || !name || !description || !commandTemplate}
            style={btnStyle(tc, "primary")}
          >
            {busy ? "Salvataggio..." : "Crea"}
          </button>
        </div>
      </div>
    </div>
  );
}

function StatusItem({ label, value, ok }: { label: string; value: string; ok: boolean }) {
  const tc = useThemeColors();
  return (
    <div>
      <div style={{ fontSize: 10, color: tc.textMuted, textTransform: "uppercase", letterSpacing: 0.5 }}>{label}</div>
      <div style={{ color: ok ? tc.success : tc.error, fontWeight: 600 }}>{value}</div>
    </div>
  );
}

function FormField({ label, children }: { label: string; children: React.ReactNode }) {
  const tc = useThemeColors();
  return (
    <label style={{ display: "block", marginBottom: 10 }}>
      <div style={{ fontSize: 11, color: tc.textSecondary, marginBottom: 4 }}>{label}</div>
      {children}
    </label>
  );
}

function inputStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    width: "100%",
    padding: "6px 8px",
    background: tc.bgInput,
    border: `1px solid ${tc.border}`,
    borderRadius: 4,
    color: tc.text,
    fontSize: 12.5,
    boxSizing: "border-box" as const,
  };
}

function btnStyle(
  tc: ReturnType<typeof useThemeColors>,
  variant: "primary" | "secondary",
) {
  return {
    padding: "5px 12px",
    background: variant === "primary" ? tc.accent : tc.bgInput,
    color: variant === "primary" ? "#fff" : tc.text,
    border: `1px solid ${variant === "primary" ? tc.accent : tc.border}`,
    borderRadius: 4,
    cursor: "pointer",
    fontSize: 12,
  };
}

function th() {
  return { textAlign: "left" as const, padding: "6px 10px", fontSize: 11, textTransform: "uppercase" as const };
}
function td() {
  return { padding: "6px 10px", verticalAlign: "top" as const };
}
function preStyle(tc: ReturnType<typeof useThemeColors>) {
  return {
    margin: "6px 0 0",
    padding: 8,
    background: tc.bgInput,
    border: `1px solid ${tc.border}`,
    borderRadius: 4,
    fontSize: 11.5,
    fontFamily: '"JetBrains Mono", "Consolas", monospace',
    maxHeight: 240,
    overflow: "auto" as const,
    whiteSpace: "pre-wrap" as const,
  };
}
