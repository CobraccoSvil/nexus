"use client";

import React from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { useThemeColors } from "../../lib/theme";
import { ExecutableCodeBlock } from "./executable-code-block";

function normalizeContent(raw: string): string {
  let s = raw;
  // react-markdown 10.x: delimitatori inline (**bold**, *italic*, __bold__, _italic_)
  // non vengono interpretati se attaccati direttamente al testo precedente senza spazio.
  // Inseriamo spazio solo dopo punteggiatura (conservativo: non tocca "Bold**resto"
  // che potrebbe essere una chiusura, ma corregge "fatto.**Ora**" e "risultato:**Bold**").
  // Il \s* cattura spazi gia' presenti e il replacement normalizza a esattamente uno.
  s = s.replace(/([.,:;!?)\]])\s*(\*{1,2})([A-Za-zÀ-ü0-9])/g, "$1 $2$3");
  s = s.replace(/([.,:;!?)\]])\s*(_{1,2})([A-Za-zÀ-ü0-9])/g, "$1 $2$3");
  // "frase.Frase" o "frase.Il" senza spazio → aggiunge doppio a-capo
  s = s.replace(/\.([A-ZÀ-Ü])/g, ".\n\n$1");
  // Stessa cosa per "frase.[Link" (markdown link dopo punto senza spazio)
  s = s.replace(/\.(\[)/g, ".\n\n$1");
  // Fix M13: pattern tipico del flow narrative agente "Sto facendo X:Procedo con Y"
  // ":Maiuscola" → ":\n\nMaiuscola" (esclude URL https:// e ftp://)
  // Negative lookbehind: non spezzare se preceduto da http/https/ftp/file/mailto
  s = s.replace(/(?<!https?|ftp|file|mailto):([A-ZÀ-Ü])/g, ":\n\n$1");
  // Stessa cosa per ":L'" / ":Un'" (italiano: apostrofo dopo maiuscola)
  s = s.replace(/(?<!https?|ftp|file|mailto):([A-ZÀ-Ü]['ʼ])/g, ":\n\n$1");
  // Frasi che indicano transizione: " Now ", " Verifico ", " Creo ", " Aspetto ", " Aggiungo ", " Installo "
  // Solo se preceduto da `.` o `:` per evitare false positive in frasi normali
  s = s.replace(/([.:])\s+(Now|Verifico|Creo|Aspetto|Aggiungo|Installo|Sto installando|Sto creando|Sto verificando|Procedo)\s+/g, "$1\n\n$2 ");
  // Evita tripli+ a-capo consecutivi creati dalla normalizzazione
  s = s.replace(/\n{3,}/g, "\n\n");
  return s;
}

/** Linguaggi shell per cui mostrare il pulsante "Esegui". */
const SHELL_LANGUAGES = new Set(["bash", "sh", "shell", "zsh", "console"]);

const remarkPluginsList = [remarkGfm];

/** Pattern file path: identifica stringhe che SEMBRANO percorsi a file di progetto.
 *
 * Matcha:
 *   - filename.ext               (es. README.md, function_report.txt)
 *   - path/to/file.ext           (es. src/main.rs, apps/web-ide/lib/api-client.ts)
 *   - path/with-dashes_and_dots/file.ext
 *
 * Estensioni supportate (ASCII-only, lowercase, max 5 chars):
 *   txt md ts tsx js jsx json yaml yml toml sql py rs go java cpp h hpp c
 *   sh bash html css scss less xml csv conf ini env dockerfile makefile etc.
 *
 * Esclude:
 *   - URL (contengono :// o iniziano con http)
 *   - Stringhe troppo lunghe (>200 char, probabile non e' un path)
 *   - Caratteri spazio (path con spazi non li gestiamo qui, niente quote)
 */
const FILE_PATH_REGEX = /^[\w][\w./-]{0,200}\.[a-zA-Z0-9]{1,8}$/;

function looksLikeFilePath(text: string): boolean {
  if (!text || text.length > 200) return false;
  if (text.includes("://") || text.startsWith("http")) return false;
  if (text.startsWith("/") && text.length < 4) return false;
  return FILE_PATH_REGEX.test(text);
}

/** Estrae testo puro da children React (ReactMarkdown passa stringhe o array). */
function extractText(node: React.ReactNode): string {
  if (typeof node === "string") return node;
  if (Array.isArray(node)) return node.map(extractText).join("");
  if (React.isValidElement(node)) {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    return extractText((node.props as any)?.children);
  }
  return "";
}

export const MarkdownBlock = React.memo(function MarkdownBlock({
  content,
  skipNormalize = false,
  projectId,
}: {
  content: string;
  skipNormalize?: boolean;
  projectId?: string;
}) {
  const tc = useThemeColors();
  const normalized = skipNormalize ? (content ?? "") : normalizeContent(content ?? "");

  const components = React.useMemo(() => ({
          p: ({ children }: { children?: React.ReactNode }) => (
            <p style={{ margin: "10px 0", lineHeight: 1.75 }}>{children}</p>
          ),
          strong: ({ children }: { children?: React.ReactNode }) => <strong>{children}</strong>,
          em: ({ children }: { children?: React.ReactNode }) => <em>{children}</em>,
          a: (({ href, children }: { href?: string; children?: React.ReactNode }) => (
            <a
              href={href}
              target="_blank"
              rel="noreferrer"
              style={{ color: tc.accent, textDecoration: "underline" }}
            >
              {children}
            </a>
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          )) as any,
          code: (({ className, children }: { className?: string; children?: React.ReactNode }) => {
            // Block code (dentro <pre>): preserva intero contenuto compreso
            // di newline e caratteri ASCII per diagrammi (es. ┌─┐│└─┘).
            // Per non perdere indentazione e a-capo, usiamo white-space: pre.
            if (className?.startsWith("language-")) {
              return (
                <code
                  className={className}
                  style={{
                    fontFamily: '"JetBrains Mono", "Consolas", monospace',
                    fontSize: 12,
                    color: tc.text,
                    whiteSpace: "pre",
                  }}
                >
                  {children}
                </code>
              );
            }
            // Inline code: se sembra un file path, lo rendiamo cliccabile.
            // Click -> dispatcha evento globale `nexus:editor:open-file` che
            // ide-shell intercetta e apre il file nel gruppo editor attivo.
            const text = extractText(children).trim();
            if (looksLikeFilePath(text)) {
              return (
                <code
                  role="button"
                  tabIndex={0}
                  title={`Apri ${text} nell'editor`}
                  onClick={(e) => {
                    e.preventDefault();
                    e.stopPropagation();
                    if (typeof window !== "undefined") {
                      window.dispatchEvent(new CustomEvent("nexus:editor:open-file", {
                        detail: { path: text },
                      }));
                    }
                  }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" || e.key === " ") {
                      e.preventDefault();
                      if (typeof window !== "undefined") {
                        window.dispatchEvent(new CustomEvent("nexus:editor:open-file", {
                          detail: { path: text },
                        }));
                      }
                    }
                  }}
                  style={{
                    background: tc.bgInput,
                    border: `1px solid ${tc.accent}`,
                    borderRadius: 4,
                    padding: "0 4px",
                    fontFamily: '"JetBrains Mono", monospace',
                    fontSize: "0.92em",
                    color: tc.accent,
                    cursor: "pointer",
                    textDecoration: "underline",
                    textDecorationStyle: "dotted",
                  }}
                >
                  {children}
                </code>
              );
            }
            // Inline code regular
            return (
              <code
                style={{
                  background: tc.bgInput,
                  border: `1px solid ${tc.border}`,
                  borderRadius: 4,
                  padding: "0 4px",
                  fontFamily: '"JetBrains Mono", monospace',
                  fontSize: "0.92em",
                  color: tc.accent,
                }}
              >
                {children}
              </code>
            );
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          }) as any,
          pre: (({ children }: { children?: React.ReactNode }) => {
            // Se projectId e' fornito, intercetta blocchi bash/sh per renderizzarli
            // come ExecutableCodeBlock con pulsante "Esegui" e stato controllato.
            if (projectId && React.isValidElement(children)) {
              const childEl = children as React.ReactElement<{
                className?: string;
                children?: React.ReactNode;
              }>;
              const className = childEl.props?.className ?? "";
              const langMatch = className.match(/^language-(\w+)/);
              if (langMatch && SHELL_LANGUAGES.has(langMatch[1])) {
                const code = extractText(childEl.props?.children).replace(/\n$/, "");
                return (
                  <ExecutableCodeBlock
                    code={code}
                    language={langMatch[1]}
                    projectId={projectId}
                    tc={tc}
                  />
                );
              }
              // Blocchi SQL → chip "Esegui nel pannello SQL" che apre il
              // pannello destro pre-compilato. Vedi listener `nexus:sql:open`
              // in ide-shell.tsx + SqlQueryPanel.
              if (langMatch && langMatch[1].toLowerCase() === "sql") {
                const code = extractText(childEl.props?.children).replace(/\n$/, "");
                return <SqlChatBlock code={code} tc={tc} />;
              }
            }
            // Fallback: rendering pre standard
            return (
              <pre
                style={{
                  background: tc.bgInput,
                  border: `1px solid ${tc.border}`,
                  borderRadius: 6,
                  padding: "10px 12px",
                  overflowX: "auto",
                  fontFamily: '"JetBrains Mono", "Consolas", monospace',
                  fontSize: 12,
                  lineHeight: 1.5,
                  color: tc.text,
                  margin: "12px 0",
                  whiteSpace: "pre",
                }}
              >
                {children}
              </pre>
            );
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          }) as any,
          h1: ({ children }: { children?: React.ReactNode }) => (
            <div style={{ fontWeight: 700, fontSize: 18, color: tc.text, margin: "18px 0 8px", borderBottom: `1px solid ${tc.border}`, paddingBottom: 4 }}>
              {children}
            </div>
          ),
          h2: ({ children }: { children?: React.ReactNode }) => (
            <div style={{ fontWeight: 700, fontSize: 16, color: tc.text, margin: "16px 0 6px", borderBottom: `1px solid ${tc.border}`, paddingBottom: 3 }}>
              {children}
            </div>
          ),
          h3: ({ children }: { children?: React.ReactNode }) => (
            <div style={{ fontWeight: 700, fontSize: 14, color: tc.text, margin: "14px 0 6px" }}>
              {children}
            </div>
          ),
          h4: ({ children }: { children?: React.ReactNode }) => (
            <div style={{ fontWeight: 600, fontSize: 13, color: tc.text, margin: "12px 0 4px" }}>
              {children}
            </div>
          ),
          ul: ({ children }: { children?: React.ReactNode }) => (
            <ul style={{ margin: "8px 0", paddingLeft: 20 }}>{children}</ul>
          ),
          ol: ({ children }: { children?: React.ReactNode }) => (
            <ol style={{ margin: "8px 0", paddingLeft: 22 }}>{children}</ol>
          ),
          li: ({ children }: { children?: React.ReactNode }) => (
            <li style={{ marginBottom: 4, lineHeight: 1.65 }}>{children}</li>
          ),
          blockquote: ({ children }: { children?: React.ReactNode }) => (
            <blockquote style={{ borderLeft: `3px solid ${tc.accent}`, paddingLeft: 12, margin: "10px 0", color: tc.textSecondary, fontStyle: "italic" }}>
              {children}
            </blockquote>
          ),
          hr: () => (
            <hr style={{ border: "none", borderTop: `1px solid ${tc.border}`, margin: "14px 0" }} />
          ),
          table: ({ children }: { children?: React.ReactNode }) => (
            <div style={{ overflowX: "auto", margin: "12px 0", border: `1px solid ${tc.border}`, borderRadius: 6 }}>
              <table style={{ borderCollapse: "collapse", width: "100%", fontSize: 12.5 }}>
                {children}
              </table>
            </div>
          ),
          thead: (({ children }: { children?: React.ReactNode }) => (
            <thead style={{ background: tc.bgInput }}>{children}</thead>
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          )) as any,
          tr: (({ children }: { children?: React.ReactNode }) => (
            <tr style={{ borderBottom: `1px solid ${tc.border}` }}>{children}</tr>
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          )) as any,
          th: (({ children }: { children?: React.ReactNode }) => (
            <th style={{ borderRight: `1px solid ${tc.border}`, padding: "8px 12px", color: tc.text, fontWeight: 700, textAlign: "left" as const }}>
              {children}
            </th>
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          )) as any,
          td: (({ children }: { children?: React.ReactNode }) => (
            <td style={{ borderRight: `1px solid ${tc.border}`, padding: "6px 12px", color: tc.text, verticalAlign: "top" as const }}>
              {children}
            </td>
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          )) as any,
  }), [tc, projectId]);

  return (
    <div style={{ lineHeight: 1.7, fontSize: 13.5 }}>
      <ReactMarkdown
        remarkPlugins={remarkPluginsList}
        components={components}
      >
        {normalized}
      </ReactMarkdown>
    </div>
  );
});

// ── SqlChatBlock: blocco SQL della chat con pulsante "Esegui nel pannello SQL"
// che dispatcha `nexus:sql:open` con il contenuto. Il listener in ide-shell
// apre il pannello SQL destro e pre-compila l'editor (vedi anche
// `nexus:sql:set-content` ascoltato da SqlQueryPanel).
//
// Niente esecuzione inline: per le query DDL serve l'archiviazione automatica
// in Knowledge Base + file migration, gestita dall'endpoint REST chiamato
// SOLO dal pannello SQL. Mostrare il risultato qui scavalcherebbe quella
// logica.
function SqlChatBlock({ code, tc }: { code: string; tc: ReturnType<typeof useThemeColors> }) {
  const trimmed = code.trim();
  const isDdl = React.useMemo(() => {
    const t = trimmed.toLowerCase();
    return (
      t.startsWith("create") ||
      t.startsWith("alter") ||
      t.startsWith("drop") ||
      t.startsWith("truncate") ||
      t.startsWith("rename")
    );
  }, [trimmed]);

  const open = React.useCallback(
    (autoRun: boolean) => {
      window.dispatchEvent(
        new CustomEvent("nexus:sql:open", { detail: { sql: trimmed, autoRun } }),
      );
    },
    [trimmed],
  );

  return (
    <div
      style={{
        margin: "12px 0",
        border: `1px solid ${tc.border}`,
        borderRadius: 6,
        overflow: "hidden",
        background: tc.bgInput,
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 8,
          padding: "4px 10px",
          background: tc.bgCard,
          borderBottom: `1px solid ${tc.border}`,
          fontSize: 11,
          color: tc.textMuted,
        }}
      >
        <span style={{ fontWeight: 600, color: tc.accent }}>SQL</span>
        {isDdl && (
          <span
            title="DDL: dopo l'esecuzione viene archiviata in Knowledge Base e come file migration."
            style={{
              padding: "1px 6px",
              background: "#7a5b00",
              color: "#fff8d6",
              borderRadius: 3,
              fontSize: 10,
            }}
          >
            schema-change
          </span>
        )}
        <button
          type="button"
          onClick={() => open(false)}
          style={{
            marginLeft: "auto",
            padding: "2px 10px",
            background: "transparent",
            color: tc.accent,
            border: `1px solid ${tc.accent}`,
            borderRadius: 4,
            cursor: "pointer",
            fontSize: 11,
          }}
          title="Apri nel pannello SQL (non esegue automaticamente)"
        >
          Apri nel pannello SQL
        </button>
        <button
          type="button"
          onClick={() => open(true)}
          style={{
            padding: "2px 10px",
            background: tc.accent,
            color: "#fff",
            border: "none",
            borderRadius: 4,
            cursor: "pointer",
            fontSize: 11,
          }}
          title="Apre il pannello SQL ed esegue subito la query"
        >
          Esegui
        </button>
      </div>
      <pre
        style={{
          margin: 0,
          padding: "8px 12px",
          overflowX: "auto",
          fontFamily: '"JetBrains Mono", "Consolas", monospace',
          fontSize: 12,
          lineHeight: 1.5,
          color: tc.text,
          whiteSpace: "pre",
        }}
      >
        {trimmed}
      </pre>
    </div>
  );
}
