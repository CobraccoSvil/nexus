"use client";

import { useEffect, useState } from "react";
import { EnvironmentCheck, getEnvironmentStatus, fixEnvironment } from "../../lib/api-client";

function statusIcon(status: EnvironmentCheck["status"]): string {
  switch (status) {
    case "ok": return "✅";
    case "warn": return "⚠️";
    case "error": return "❌";
    case "loading": return "⏳";
    default: return "❓";
  }
}

function statusColor(status: EnvironmentCheck["status"]): string {
  switch (status) {
    case "ok": return "var(--color-success)";
    case "warn": return "#f59e0b";
    case "error": return "var(--color-error)";
    case "loading": return "var(--color-textMuted)";
    default: return "var(--color-textMuted)";
  }
}

interface FixButtonProps {
  label: string;
  action: string;
  checkId: string;
  fixLoading: Record<string, boolean>;
  fixOutputs: Record<string, string>;
  onFix: (action: string, checkId: string) => Promise<void>;
}

function FixButton({ label, action, checkId, fixLoading, fixOutputs, onFix }: FixButtonProps) {
  const loading = fixLoading[checkId] ?? false;
  const output = fixOutputs[checkId];

  return (
    <div style={{ marginTop: 8 }}>
      <button
        onClick={() => void onFix(action, checkId)}
        disabled={loading}
        style={{
          padding: "4px 12px",
          fontSize: 12,
          borderRadius: 6,
          border: "1px solid var(--color-border)",
          background: loading ? "var(--color-bgActive)" : "var(--color-bgSidebar)",
          color: "var(--color-text)",
          cursor: loading ? "not-allowed" : "pointer",
          opacity: loading ? 0.7 : 1,
        }}
      >
        {loading ? "⏳ In corso..." : label}
      </button>
      {output && (
        <pre
          style={{
            marginTop: 8,
            padding: "8px 12px",
            background: "var(--color-bgCard)",
            border: "1px solid var(--color-border)",
            borderRadius: 6,
            fontSize: 11,
            overflowX: "auto",
            whiteSpace: "pre-wrap",
            wordBreak: "break-all",
            maxHeight: 200,
            overflowY: "auto",
            color: "var(--color-textSecondary)",
          }}
        >
          {output}
        </pre>
      )}
    </div>
  );
}

interface CopyCommandProps {
  checkId: string;
  fixLoading: Record<string, boolean>;
  fixOutputs: Record<string, string>;
  onFix: (action: string, checkId: string) => Promise<void>;
}

function CopyCommand({ checkId, fixLoading, fixOutputs, onFix }: CopyCommandProps) {
  const [expanded, setExpanded] = useState(false);
  const loading = fixLoading[checkId] ?? false;
  const output = fixOutputs[checkId];

  return (
    <div style={{ marginTop: 8 }}>
      <button
        onClick={async () => {
          if (!output) {
            await onFix("get_system_deps_command", checkId);
          }
          setExpanded((v) => !v);
        }}
        disabled={loading}
        style={{
          padding: "4px 12px",
          fontSize: 12,
          borderRadius: 6,
          border: "1px solid var(--color-border)",
          background: "var(--color-bgSidebar)",
          color: "var(--color-text)",
          cursor: "pointer",
        }}
      >
        {loading ? "⏳" : "📋 Mostra comando"}
      </button>
      {expanded && output && (
        <div style={{ marginTop: 8 }}>
          <pre
            style={{
              padding: "8px 12px",
              background: "var(--color-bgCard)",
              border: "1px solid var(--color-border)",
              borderRadius: 6,
              fontSize: 11,
              whiteSpace: "pre-wrap",
              wordBreak: "break-all",
              color: "var(--color-textSecondary)",
            }}
          >
            {output}
          </pre>
          <button
            onClick={() => void navigator.clipboard.writeText(output)}
            style={{
              marginTop: 4,
              padding: "2px 10px",
              fontSize: 11,
              borderRadius: 4,
              border: "1px solid var(--color-border)",
              background: "var(--color-bgSidebar)",
              color: "var(--color-textMuted)",
              cursor: "pointer",
            }}
          >
            Copia
          </button>
        </div>
      )}
    </div>
  );
}

function CheckRow({
  check,
  fixLoading,
  fixOutputs,
  onFix,
  onSudoInstall,
}: {
  check: EnvironmentCheck;
  fixLoading: Record<string, boolean>;
  fixOutputs: Record<string, string>;
  onFix: (action: string, checkId: string) => Promise<void>;
  onSudoInstall: (action: string, checkId: string) => void;
}) {
  const showFix = check.status === "error" || check.status === "warn";

  return (
    <div
      style={{
        padding: "12px 16px",
        borderBottom: "1px solid var(--color-border)",
        background: "transparent",
      }}
    >
      <div style={{ display: "flex", alignItems: "flex-start", gap: 10 }}>
        <span style={{ fontSize: 16, lineHeight: "20px", flexShrink: 0 }}>
          {statusIcon(check.status)}
        </span>
        <div style={{ flex: 1 }}>
          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
            <span style={{ fontWeight: 600, fontSize: 13, color: "var(--color-text)" }}>
              {check.label}
            </span>
            <span
              style={{
                fontSize: 12,
                color: statusColor(check.status),
                fontWeight: check.status !== "ok" ? 500 : 400,
              }}
            >
              {check.detail}
            </span>
          </div>

          {showFix && check.id === "playwright_libs" && (
            <div style={{ marginTop: 8, display: "flex", gap: 8, flexWrap: "wrap" }}>
              <button
                onClick={() => onSudoInstall("install_system_deps", check.id)}
                disabled={fixLoading[check.id] ?? false}
                style={{
                  padding: "4px 12px",
                  fontSize: 12,
                  borderRadius: 6,
                  border: "1px solid var(--color-border)",
                  background: "var(--color-bgSidebar)",
                  color: "var(--color-text)",
                  cursor: "pointer",
                }}
              >
                🔧 Installa auto
              </button>
              <CopyCommand
                checkId={`${check.id}_cmd`}
                fixLoading={fixLoading}
                fixOutputs={fixOutputs}
                onFix={onFix}
              />
              {fixOutputs[check.id] && (
                <pre
                  style={{
                    width: "100%",
                    marginTop: 8,
                    padding: "8px 12px",
                    background: "var(--color-bgCard)",
                    border: "1px solid var(--color-border)",
                    borderRadius: 6,
                    fontSize: 11,
                    overflowX: "auto",
                    whiteSpace: "pre-wrap",
                    wordBreak: "break-all",
                    maxHeight: 200,
                    overflowY: "auto",
                    color: "var(--color-textSecondary)",
                  }}
                >
                  {fixOutputs[check.id]}
                </pre>
              )}
            </div>
          )}

          {showFix && check.id === "playwright_browser" && (
            <FixButton
              label="⬇️ Installa Chromium"
              action="install_playwright_browsers"
              checkId={check.id}
              fixLoading={fixLoading}
              fixOutputs={fixOutputs}
              onFix={onFix}
            />
          )}

          {showFix && check.id === "migrations" && (
            <FixButton
              label="Esegui migrazioni"
              action="run_migrations"
              checkId={check.id}
              fixLoading={fixLoading}
              fixOutputs={fixOutputs}
              onFix={onFix}
            />
          )}

          {showFix && check.id === "migrations_sqlx_missing" && (
            <FixButton
              label="Installa sqlx-cli"
              action="install_sqlx_cli"
              checkId={check.id}
              fixLoading={fixLoading}
              fixOutputs={fixOutputs}
              onFix={onFix}
            />
          )}
        </div>
      </div>
    </div>
  );
}

export function EnvironmentPanel() {
  const [checks, setChecks] = useState<EnvironmentCheck[]>([]);
  const [loading, setLoading] = useState(false);
  const [fixOutputs, setFixOutputs] = useState<Record<string, string>>({});
  const [fixLoading, setFixLoading] = useState<Record<string, boolean>>({});
  const [lastCheck, setLastCheck] = useState<Date | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sudoModal, setSudoModal] = useState<{ open: boolean; action: string; checkId: string } | null>(null);
  const [sudoPassword, setSudoPassword] = useState("");
  const [sudoError, setSudoError] = useState("");

  const refresh = async () => {
    setLoading(true);
    setChecks((prev) =>
      prev.length > 0
        ? prev.map((c) => ({ ...c, status: "loading" as const }))
        : []
    );
    setError(null);
    try {
      const res = await getEnvironmentStatus();
      setChecks(res.checks);
      setLastCheck(new Date());
    } catch (e) {
      setError(e instanceof Error ? e.message : "Errore nel recupero dello stato ambiente");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void refresh();
  }, []);

  const runFix = async (action: string, checkId: string) => {
    setFixLoading((prev) => ({ ...prev, [checkId]: true }));
    try {
      const res = await fixEnvironment(action);
      setFixOutputs((prev) => ({ ...prev, [checkId]: res.output }));
      // Refresh checks dopo fix
      await refresh();
    } catch (e) {
      setFixOutputs((prev) => ({
        ...prev,
        [checkId]: e instanceof Error ? e.message : "Errore sconosciuto",
      }));
    } finally {
      setFixLoading((prev) => ({ ...prev, [checkId]: false }));
    }
  };

  const handleSudoConfirm = async () => {
    if (!sudoModal || !sudoPassword) return;
    setSudoError("");
    setFixLoading(prev => ({ ...prev, [sudoModal.checkId]: true }));
    try {
      const res = await fixEnvironment(sudoModal.action, sudoPassword);
      if (!res.ok && (res.output.includes("incorrect password") || res.output.includes("authentication failure") || res.output.includes("Sorry, try again"))) {
        setSudoError("Password errata. Riprova.");
        return;
      }
      setFixOutputs(prev => ({ ...prev, [sudoModal.checkId]: res.output }));
      setSudoModal(null);
      setSudoPassword("");
      await refresh();
    } catch {
      setSudoError("Errore durante l'installazione.");
    } finally {
      setFixLoading(prev => ({ ...prev, [sudoModal!.checkId]: false }));
    }
  };

  const okCount = checks.filter((c) => c.status === "ok").length;
  const errorCount = checks.filter((c) => c.status === "error").length;
  const warnCount = checks.filter((c) => c.status === "warn").length;

  return (
    <div>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 20 }}>
        <div>
          <h2 style={{ fontSize: 18, fontWeight: 600, margin: 0, color: "var(--color-text)" }}>
            Stato ambiente Nexus
          </h2>
          {lastCheck && (
            <p style={{ fontSize: 12, color: "var(--color-textMuted)", margin: "4px 0 0" }}>
              Ultimo aggiornamento: {lastCheck.toLocaleTimeString()}
              {checks.length > 0 && (
                <span style={{ marginLeft: 12 }}>
                  <span style={{ color: "#22c55e" }}>{okCount} ok</span>
                  {warnCount > 0 && <span style={{ color: "#f59e0b", marginLeft: 8 }}>{warnCount} avvisi</span>}
                  {errorCount > 0 && <span style={{ color: "var(--color-error)", marginLeft: 8 }}>{errorCount} errori</span>}
                </span>
              )}
            </p>
          )}
        </div>
        <button
          onClick={() => void refresh()}
          disabled={loading}
          style={{
            padding: "6px 16px",
            fontSize: 13,
            borderRadius: 8,
            border: "1px solid var(--color-border)",
            background: loading ? "var(--color-bgActive)" : "var(--color-bgSidebar)",
            color: "var(--color-text)",
            cursor: loading ? "not-allowed" : "pointer",
            opacity: loading ? 0.7 : 1,
          }}
        >
          {loading ? "⏳ Aggiornamento..." : "🔄 Aggiorna"}
        </button>
      </div>

      {error && (
        <div
          style={{
            padding: "10px 16px",
            background: "#2d1215",
            border: "1px solid var(--color-error)",
            borderRadius: 8,
            color: "var(--color-error)",
            fontSize: 13,
            marginBottom: 16,
          }}
        >
          {error}
        </div>
      )}

      {checks.length === 0 && !loading && !error && (
        <div style={{ padding: 40, textAlign: "center", color: "var(--color-textMuted)", fontSize: 13 }}>
          Nessun dato disponibile. Clicca Aggiorna per caricare.
        </div>
      )}

      {checks.length > 0 && (
        <div
          style={{
            border: "1px solid var(--color-border)",
            borderRadius: 10,
            overflow: "hidden",
          }}
        >
          {checks.map((check) => (
            <CheckRow
              key={check.id}
              check={check}
              fixLoading={fixLoading}
              fixOutputs={fixOutputs}
              onFix={runFix}
              onSudoInstall={(action, checkId) => setSudoModal({ open: true, action, checkId })}
            />
          ))}
        </div>
      )}

      {sudoModal?.open && (
        <div style={{
          position: "fixed", inset: 0, background: "rgba(0,0,0,0.6)",
          display: "flex", alignItems: "center", justifyContent: "center",
          zIndex: 1000,
        }}>
          <div style={{
            background: "var(--color-bgCard)", border: "1px solid var(--color-border)",
            borderRadius: 12, padding: 24, width: 380, maxWidth: "90vw",
          }}>
            <h3 style={{ margin: "0 0 8px", color: "var(--color-text)" }}>Password sudo richiesta</h3>
            <p style={{ color: "var(--color-textMuted)", fontSize: 13, margin: "0 0 16px" }}>
              Per installare le dipendenze di sistema è necessaria la password sudo del server.
              La password non viene salvata.
            </p>
            <input
              type="password"
              value={sudoPassword}
              onChange={e => { setSudoPassword(e.target.value); setSudoError(""); }}
              onKeyDown={e => { if (e.key === "Enter") void handleSudoConfirm(); }}
              placeholder="Password sudo..."
              autoFocus
              style={{
                width: "100%", boxSizing: "border-box",
                padding: "8px 12px", borderRadius: 6,
                border: `1px solid ${sudoError ? "var(--color-error)" : "var(--color-border)"}`,
                background: "var(--color-bgSidebar)", color: "var(--color-text)", fontSize: 13,
                marginBottom: sudoError ? 6 : 16,
              }}
            />
            {sudoError && <div style={{ color: "var(--color-error)", fontSize: 12, marginBottom: 12 }}>{sudoError}</div>}
            <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
              <button
                onClick={() => { setSudoModal(null); setSudoPassword(""); setSudoError(""); }}
                style={{ padding: "6px 14px", borderRadius: 6, border: "1px solid var(--color-border)", background: "none", color: "var(--color-textMuted)", cursor: "pointer" }}
              >
                Annulla
              </button>
              <button
                onClick={() => void handleSudoConfirm()}
                disabled={!sudoPassword || (fixLoading[sudoModal.checkId] ?? false)}
                style={{ padding: "6px 14px", borderRadius: 6, border: "none", background: "var(--color-accent)", color: "#fff", cursor: "pointer" }}
              >
                {(fixLoading[sudoModal.checkId] ?? false) ? "Installando..." : "Installa"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
