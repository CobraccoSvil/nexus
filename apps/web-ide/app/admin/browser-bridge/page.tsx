"use client";

// Sezione admin: gestione installazione automatica dell'estensione Chrome
// "IDEAI Browser Bridge". Recupera lo stato dal daemon browser-bridge-mcp via
// /api/admin/browser-bridge/info e offre i link per scaricare gli script di
// installazione policy (Windows / Linux) generati a runtime dal daemon.

import { useCallback, useEffect, useState } from "react";
import { useThemeColors } from "../../../lib/theme";

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

export default function BrowserBridgePage() {
  const tc = useThemeColors();
  const [info, setInfo]           = useState<Info | null>(null);
  const [loading, setLoading]     = useState(true);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const [dlError, setDlError]     = useState<string | null>(null);

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

  // ----- stili -----
  const card: React.CSSProperties = {
    background: tc.bgCard,
    border: `1px solid ${tc.border}`,
    borderRadius: 10,
    padding: 20,
    marginBottom: 16,
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
  const btn = (primary: boolean, disabled: boolean): React.CSSProperties => ({
    padding: "9px 16px",
    borderRadius: 6,
    border: `1px solid ${tc.accent}`,
    background: primary ? tc.accent : "transparent",
    color: primary ? "#fff" : tc.accent,
    cursor: disabled ? "not-allowed" : "pointer",
    fontWeight: 600,
    fontSize: 13,
    opacity: disabled ? 0.45 : 1,
    transition: "opacity 0.15s",
    fontFamily: "inherit",
  });

  const ready = !!(info && info.extension_id && info.crx_available && !info.error);

  return (
    <div>
      <h1 style={{ fontSize: 20, fontWeight: 700, marginBottom: 6, color: tc.text }}>
        Browser Bridge
      </h1>
      <p style={{ color: tc.textMuted, fontSize: 13, marginBottom: 20 }}>
        Installa l&apos;estensione Chrome che permette a Nexus di leggere console, errori e
        rete del browser e di guidarne la navigazione in test autonomi.
        L&apos;installazione e&apos; silenziosa via Chrome Enterprise Policy: basta un
        singolo lancio &quot;Esegui come amministratore&quot; dello script generato.
      </p>

      {/* --- stato daemon --- */}
      <div style={card}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 14 }}>
          <strong style={{ fontSize: 14, color: tc.text }}>Stato daemon</strong>
          <button onClick={fetchInfo} style={btn(false, loading)} disabled={loading}>
            {loading ? "Aggiorno..." : "Ricarica"}
          </button>
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
            style={btn(true, !ready)}
            disabled={!ready}
            title={ready ? "Scarica script installazione policy Chrome (Windows)" : "Daemon non pronto o CRX assente"}
          >
            Scarica .ps1 (Windows)
          </button>
          <button
            onClick={() => download(PROXY_SH, "install-browser-bridge.sh")}
            style={btn(false, !ready)}
            disabled={!ready}
            title={ready ? "Scarica script installazione policy Chrome (Linux)" : "Daemon non pronto o CRX assente"}
          >
            Scarica .sh (Linux)
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

      {/* --- dopo l'installazione --- */}
      <div style={card}>
        <strong style={{ fontSize: 14, color: tc.text }}>Dopo l&apos;installazione</strong>
        <ol style={{ color: tc.textMuted, fontSize: 13, margin: "10px 0 0 18px", lineHeight: 1.8 }}>
          <li>Riavvia Chrome (o apri un&apos;istanza nuova).</li>
          <li>L&apos;icona <strong>IDEAI Browser Bridge</strong> appare nella toolbar.</li>
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
