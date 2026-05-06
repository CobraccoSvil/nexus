"use client";

// Sezione admin: gestione installazione automatica dell'estensione Chrome
// "Nexus Browser Bridge". Recupera lo stato dal daemon browser-bridge-mcp via
// /api/admin/browser-bridge/info e offre i link per scaricare gli script di
// installazione policy (Windows / Linux) generati a runtime dal daemon.

import { useCallback, useEffect, useState } from "react";
import { useThemeColors } from "../../../lib/theme";
import { createMcpServer } from "../../../lib/api-client";

type Info = {
  extension_id: string | null;
  version: string | null;
  crx_available: boolean;
  crx_url: string;
  update_url: string;
  install_windows_url: string;
  install_linux_url: string;
  error: string | null;
};

const PROXY_INFO = "/api/admin/browser-bridge/info";
const PROXY_PS1 = "/api/admin/browser-bridge/install.ps1";
const PROXY_SH  = "/api/admin/browser-bridge/install.sh";
const PROXY_UNINSTALL_PS1 = "/api/admin/browser-bridge/uninstall.ps1";
const PROXY_UNINSTALL_SH  = "/api/admin/browser-bridge/uninstall.sh";
const MCP_URL = "http://127.0.0.1:4055/mcp";

export default function BrowserBridgePage() {
  const tc = useThemeColors();
  const [info, setInfo]           = useState<Info | null>(null);
  const [loading, setLoading]     = useState(true);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const [dlError, setDlError]     = useState<string | null>(null);
  const [registerBusy, setRegisterBusy] = useState(false);
  const [registerMsg, setRegisterMsg] = useState<string | null>(null);

  const fetchInfo = useCallback(async () => {
    setLoading(true);
    setFetchError(null);
    try {
      const r = await fetch(PROXY_INFO, { credentials: "include" });
      if (!r.ok) throw new Error(`${r.status} ${r.statusText}`);
      setInfo((await r.json()) as Info);
    } catch (e) {
      setFetchError(e instanceof Error ? e.message : String(e));
      setInfo(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { void fetchInfo(); }, [fetchInfo]);

  // Scarica via fetch + blob: unico modo affidabile con cookie di sessione.
  const download = useCallback(async (url: string, filename: string) => {
    setDlError(null);
    try {
      const r = await fetch(url, { credentials: "include" });
      if (!r.ok) throw new Error(`${r.status} ${r.statusText} — il daemon e' raggiungibile?`);
      const text = await r.text();
      if (!text.trim()) throw new Error("risposta vuota dal daemon");

      // Metodo 1: data-URL inline (funziona anche con popup-blocker).
      const dataUrl =
        "data:text/plain;charset=utf-8," + encodeURIComponent(text);
      const a = document.createElement("a");
      a.href = dataUrl;
      a.download = filename;
      a.style.display = "none";
      document.body.appendChild(a);
      a.click();
      // Piccola pausa prima di rimuovere: alcuni browser ignorano click sincroni.
      setTimeout(() => {
        document.body.removeChild(a);
      }, 200);
    } catch (e) {
      setDlError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const registerAsMcp = useCallback(async () => {
    setRegisterMsg(null);
    setRegisterBusy(true);
    try {
      await createMcpServer({
        name: "Nexus Browser Bridge (local)",
        description: "Bridge locale verso estensione Chrome (daemon browser-bridge-mcp).",
        transport: "http",
        url: MCP_URL,
        scope: "user",
      });
      setRegisterMsg("Registrato come MCP. Ora lo trovi in Template Prompt → MCP Tools → Tool disponibili.");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      // Se esiste già, l'API può rispondere con 409/400: mostriamo un messaggio utile.
      setRegisterMsg(`Registrazione MCP non riuscita: ${msg}`);
    } finally {
      setRegisterBusy(false);
    }
  }, []);

  // ----- stili -----
  const headerWrap: React.CSSProperties = {
    border: `1px solid ${tc.border}`,
    borderRadius: 14,
    padding: 16,
    background: `radial-gradient(1000px 520px at 10% 10%, ${tc.accent}22, transparent 55%),
                 radial-gradient(900px 440px at 90% 0%, #22c55e14, transparent 55%),
                 ${tc.bgCard}`,
    marginBottom: 14,
  };
  const logo: React.CSSProperties = {
    width: 40,
    height: 40,
    borderRadius: 12,
    background: tc.accent,
    color: "#fff",
    display: "grid",
    placeItems: "center",
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
    fontWeight: 900,
    fontSize: 20,
    boxShadow: `0 16px 40px ${tc.accent}30`,
    flexShrink: 0,
  };
  const card: React.CSSProperties = {
    background: tc.bgCard,
    border: `1px solid ${tc.border}`,
    borderRadius: 10,
    padding: 20,
    marginBottom: 16,
  };
  const pill: React.CSSProperties = {
    fontSize: 11,
    padding: "4px 8px",
    borderRadius: 999,
    border: `1px solid ${tc.border}`,
    background: tc.bgInput,
    color: tc.textMuted,
    fontWeight: 700,
    textTransform: "uppercase",
  };
  const codeBox: React.CSSProperties = {
    background: tc.bgInput,
    border: `1px solid ${tc.border}`,
    borderRadius: 6,
    padding: "8px 10px",
    fontFamily: "'JetBrains Mono', monospace",
    fontSize: 12,
    color: tc.text,
    overflowX: "auto",
    whiteSpace: "nowrap",
  };
  const urlBox: React.CSSProperties = {
    ...codeBox,
    whiteSpace: "normal",
    wordBreak: "break-all",
    overflowX: "hidden",
  };
  const btn = (
    variant: "primary" | "secondary",
    disabled: boolean,
  ): React.CSSProperties => ({
    padding: "9px 16px",
    borderRadius: 6,
    border: `1px solid ${variant === "primary" ? tc.accent : tc.border}`,
    background: variant === "primary" ? tc.accent : tc.bgInput,
    color: variant === "primary" ? "#fff" : tc.text,
    cursor: disabled ? "not-allowed" : "pointer",
    fontWeight: 600,
    fontSize: 13,
    opacity: disabled ? 0.45 : 1,
    transition: "opacity 0.15s",
    fontFamily: "inherit",
  });

  const ready = !!(info && info.extension_id && info.crx_available && !info.error);
  const daemonReachable = !!(info && !info.error);
  const healthUrl = "http://127.0.0.1:4055/health";

  return (
    <div>
      <div style={headerWrap}>
        <div style={{ display: "flex", gap: 12, alignItems: "center", flexWrap: "wrap" }}>
          <div style={logo}>N</div>
          <div style={{ minWidth: 240, flex: 1 }}>
            <div style={{ fontSize: 18, fontWeight: 800, color: tc.text, lineHeight: 1.15 }}>
              Nexus Browser Bridge
            </div>
            <div style={{ color: tc.textMuted, fontSize: 13, marginTop: 4, lineHeight: 1.45 }}>
              Estensione Chrome che permette a Nexus di leggere <strong>console</strong>, <strong>errori</strong> e{" "}
              <strong>rete</strong> del browser e guidare la navigazione nei test.
              Installazione silenziosa via <strong>Chrome Enterprise Policy</strong>.
            </div>
          </div>
          <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
            <span
              style={{
                ...pill,
                border: `1px solid ${daemonReachable ? "#22c55e55" : tc.border}`,
                background: daemonReachable ? "#22c55e14" : tc.bgInput,
                color: daemonReachable ? "#16a34a" : tc.textMuted,
              }}
              title={daemonReachable ? "Daemon raggiungibile" : "Daemon non raggiungibile"}
            >
              {daemonReachable ? "daemon: ok" : "daemon: down"}
            </span>
            <span
              style={{
                ...pill,
                border: `1px solid ${ready ? "#22c55e55" : tc.border}`,
                background: ready ? "#22c55e14" : tc.bgInput,
                color: ready ? "#16a34a" : tc.textMuted,
              }}
              title={ready ? "CRX e chiave presenti" : "CRX/chiave mancanti"}
            >
              {ready ? "crx: pronto" : "crx: non pronto"}
            </span>
            <button onClick={fetchInfo} style={btn("secondary", loading)} disabled={loading}>
              {loading ? "Aggiorno..." : "Ricarica"}
            </button>
          </div>
        </div>
      </div>

      {/* --- stato daemon --- */}
      <div style={card}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 14 }}>
          <strong style={{ fontSize: 14, color: tc.text }}>Stato daemon</strong>
          <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
            <span style={{ fontSize: 12, color: tc.textMuted }}>
              endpoint: <code style={codeBox}>http://127.0.0.1:4055</code>
            </span>
            <button
              type="button"
              onClick={() => void navigator.clipboard?.writeText("http://127.0.0.1:4055")}
              style={btn("secondary", false)}
              title="Copia URL base"
            >
              Copia
            </button>
          </div>
        </div>

        {fetchError && (
          <div style={{ color: "#c33", fontSize: 13, marginBottom: 8 }}>
            Impossibile contattare browser-bridge-mcp: {fetchError}
            <div style={{ color: tc.textMuted, fontSize: 12, marginTop: 6 }}>
              Verifica che il daemon sia avviato ({" "}
              <code style={codeBox}>./deploy/deploy-local.sh --rust</code>
              {" "}) e in ascolto su <code>127.0.0.1:4055</code>.
            </div>
          </div>
        )}

        {info && !info.error && (
          <div style={{ display: "grid", gridTemplateColumns: "140px 1fr", rowGap: 10, columnGap: 16, fontSize: 13, alignItems: "center" }}>
            <span style={{ color: tc.textMuted }}>Extension ID</span>
            <code style={codeBox}>{info.extension_id ?? "—"}</code>
            <span style={{ color: tc.textMuted }}>Versione</span>
            <code style={codeBox}>{info.version ?? "—"}</code>
            <span style={{ color: tc.textMuted }}>CRX disponibile</span>
            <span style={{ color: info.crx_available ? "#2a7" : "#c33", fontWeight: 600 }}>
              {info.crx_available ? "si" : "no — esegui pack.ps1"}
            </span>
            <span style={{ color: tc.textMuted }}>Update URL</span>
            <code style={codeBox}>{info.update_url}</code>
          </div>
        )}

        {info?.error && (
          <div style={{ color: "#c33", fontSize: 13 }}>
            Errore lato daemon: {info.error}
            <div style={{ color: tc.textMuted, fontSize: 12, marginTop: 6 }}>
              Genera chiave e CRX:{" "}
              <code>powershell -ExecutionPolicy Bypass -File apps\browser-bridge-extension\pack.ps1</code>
            </div>
          </div>
        )}

        {daemonReachable && (
          <div style={{ marginTop: 14, display: "grid", gap: 8 }}>
            <div style={{ fontSize: 12, color: tc.textMuted }}>
              Per usare questo Bridge anche nella logica MCP (tool disponibili, auto-assegna, runtime tool search/call),
              registralo come connettore MCP HTTP.
            </div>
            <div style={{ display: "grid", gridTemplateColumns: "72px 1fr auto", gap: 8, alignItems: "center" }}>
              <span style={{ ...pill, textTransform: "none", fontWeight: 600 }}>MCP</span>
              <code style={urlBox}>{MCP_URL}</code>
              <button
                type="button"
                onClick={() => void navigator.clipboard?.writeText(MCP_URL)}
                style={btn("secondary", false)}
                title="Copia URL MCP"
              >
                Copia
              </button>
            </div>
            <div style={{ display: "flex", gap: 10, flexWrap: "wrap", alignItems: "center" }}>
              <button
                onClick={registerAsMcp}
                style={btn("primary", registerBusy)}
                disabled={registerBusy}
                title="Crea un MCP server 'Nexus Browser Bridge (local)' puntato al daemon locale"
              >
                {registerBusy ? "Registro..." : "Registra come MCP"}
              </button>
              {registerMsg && (
                <span style={{ fontSize: 12, color: registerMsg.startsWith("Registrato") ? "#16a34a" : "#c33" }}>
                  {registerMsg}
                </span>
              )}
            </div>
          </div>
        )}
      </div>

      {/* --- download script --- */}
      <div style={card}>
        <strong style={{ fontSize: 14, color: tc.text }}>Installazione automatica</strong>
        <p style={{ color: tc.textMuted, fontSize: 13, margin: "8px 0 14px" }}>
          Scarica lo script per il sistema operativo del browser target, lancialo con
          privilegi di amministratore (UAC su Windows, sudo su Linux) e riavvia Chrome.
          L&apos;estensione viene installata senza interazione aggiuntiva.
        </p>

        {dlError && (
          <div style={{ color: "#c33", fontSize: 12, marginBottom: 10, padding: "6px 10px", background: tc.bgInput, borderRadius: 6 }}>
            Errore download: {dlError}
          </div>
        )}

        <div style={{ display: "flex", gap: 10, flexWrap: "wrap" }}>
          <button
            onClick={() => download(PROXY_PS1, "install-browser-bridge.ps1")}
            style={btn("primary", !ready)}
            disabled={!ready}
            title={ready ? "Scarica script installazione policy Chrome (Windows)" : "Daemon non pronto o CRX assente"}
          >
            Scarica .ps1 (Windows)
          </button>
          <button
            onClick={() => download(PROXY_SH, "install-browser-bridge.sh")}
            style={btn("secondary", !ready)}
            disabled={!ready}
            title={ready ? "Scarica script installazione policy Chrome (Linux)" : "Daemon non pronto o CRX assente"}
          >
            Scarica .sh (Linux)
          </button>
        </div>

        <div style={{ marginTop: 12, display: "flex", gap: 10, flexWrap: "wrap" }}>
          <button
            onClick={() => download(PROXY_UNINSTALL_PS1, "uninstall-browser-bridge.ps1")}
            style={btn("secondary", !daemonReachable)}
            disabled={!daemonReachable}
            title={daemonReachable ? "Scarica script rimozione policy Chrome (Windows)" : "Daemon non raggiungibile"}
          >
            Script rimozione .ps1
          </button>
          <button
            onClick={() => download(PROXY_UNINSTALL_SH, "uninstall-browser-bridge.sh")}
            style={btn("secondary", !daemonReachable)}
            disabled={!daemonReachable}
            title={daemonReachable ? "Scarica script rimozione policy Chrome (Linux)" : "Daemon non raggiungibile"}
          >
            Script rimozione .sh
          </button>
        </div>

        {!ready && !fetchError && !loading && (
          <p style={{ color: tc.textMuted, fontSize: 12, marginTop: 10 }}>
            I pulsanti si attivano quando il daemon e&apos; raggiungibile, la chiave
            RSA e il file .crx sono presenti in{" "}
            <code>apps/browser-bridge-extension/dist/</code>.
          </p>
        )}

        {ready && (
          <details style={{ marginTop: 14, fontSize: 12, color: tc.textMuted }}>
            <summary style={{ cursor: "pointer", userSelect: "none" }}>
              Cosa scrive lo script Windows (espandi)
            </summary>
            <ul style={{ margin: "8px 0 0 18px", lineHeight: 1.7 }}>
              <li>
                Scrive <code>HKLM\Software\Policies\Google\Chrome\ExtensionInstallForcelist</code>
              </li>
              <li>
                Aggiunge la voce{" "}
                <code style={{ wordBreak: "break-all" }}>
                  {info?.extension_id};{info?.update_url}
                </code>
              </li>
              <li>
                Al riavvio Chrome polla <code>update.xml</code> dal daemon, scarica il{" "}
                <code>.crx</code> e installa automaticamente
              </li>
              <li>L&apos;utente non puo&apos; disinstallare finche&apos; la policy e&apos; attiva</li>
            </ul>
          </details>
        )}
      </div>

      {/* --- diagnostica --- */}
      <div style={card}>
        <strong style={{ fontSize: 14, color: tc.text }}>Diagnostica rapida</strong>
        <p style={{ color: tc.textMuted, fontSize: 12, margin: "8px 0 12px" }}>
          Link utili per verificare subito daemon, update manifest e CRX.
        </p>

        <div style={{ display: "grid", gap: 10 }}>
          <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
            <span style={{ ...pill, textTransform: "none", fontWeight: 600 }}>Health</span>
            <code style={codeBox}>{healthUrl}</code>
            <button
              type="button"
              onClick={() => void navigator.clipboard?.writeText(healthUrl)}
              style={btn("secondary", false)}
            >
              Copia
            </button>
            <a href={healthUrl} target="_blank" rel="noreferrer" style={{ fontSize: 12, color: tc.accent }}>
              Apri
            </a>
          </div>

          {info?.update_url && (
            <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
              <span style={{ ...pill, textTransform: "none", fontWeight: 600 }}>Update XML</span>
              <code style={codeBox}>{info.update_url}</code>
              <button
                type="button"
                onClick={() => void navigator.clipboard?.writeText(info.update_url)}
                style={btn("secondary", false)}
              >
                Copia
              </button>
              <a href={info.update_url} target="_blank" rel="noreferrer" style={{ fontSize: 12, color: tc.accent }}>
                Apri
              </a>
            </div>
          )}

          {info?.crx_url && (
            <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
              <span style={{ ...pill, textTransform: "none", fontWeight: 600 }}>CRX</span>
              <code style={codeBox}>{info.crx_url}</code>
              <button
                type="button"
                onClick={() => void navigator.clipboard?.writeText(info.crx_url)}
                style={btn("secondary", false)}
              >
                Copia
              </button>
              <a href={info.crx_url} target="_blank" rel="noreferrer" style={{ fontSize: 12, color: tc.accent }}>
                Apri
              </a>
            </div>
          )}
        </div>
      </div>

      {/* --- dopo l'installazione --- */}
      <div style={card}>
        <strong style={{ fontSize: 14, color: tc.text }}>Dopo l&apos;installazione</strong>
        <ol style={{ color: tc.textMuted, fontSize: 13, margin: "10px 0 0 18px", lineHeight: 1.8 }}>
          <li>Riavvia Chrome (o apri un&apos;istanza nuova).</li>
          <li>L&apos;icona <strong>Nexus Browser Bridge</strong> appare nella toolbar.</li>
          <li>
            Cliccala: incolla il token da{" "}
            <code>~/.ideai/browser-bridge.token</code> e premi{" "}
            <strong>Salva e riconnetti</strong>.
          </li>
          <li>
            Apri il tab da automatizzare e clicca{" "}
            <strong>Attach tab corrente</strong>.
          </li>
          <li>
            Da Nexus sono disponibili i tool MCP:{" "}
            <code>browser.navigate</code>, <code>click</code>, <code>fill</code>,{" "}
            <code>console_logs</code>, <code>network_log</code>, <code>eval</code>,{" "}
            <code>screenshot</code>, <code>snapshot_dom</code>.
          </li>
        </ol>
      </div>
    </div>
  );
}
