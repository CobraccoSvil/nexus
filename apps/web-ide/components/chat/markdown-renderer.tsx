"use client";

import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { useThemeColors } from "../../lib/theme";

function normalizeContent(raw: string): string {
  let s = raw;
  // "frase.Frase" o "frase.Il" senza spazio → aggiunge doppio a-capo
  s = s.replace(/\.([A-ZÀ-Ü])/g, ".\n\n$1");
  // Stessa cosa per "frase.[Link" (markdown link dopo punto senza spazio)
  s = s.replace(/\.(\[)/g, ".\n\n$1");
  // Evita tripli+ a-capo consecutivi creati dalla normalizzazione
  s = s.replace(/\n{3,}/g, "\n\n");
  return s;
}

export function MarkdownBlock({
  content,
  skipNormalize = false,
}: {
  content: string;
  /** Disabilita normalizeContent (utile per file .md gia' formattati con
   *  tabelle, blocchi codice e diagrammi ASCII che la normalizzazione
   *  rompe inserendo a-capo dopo "frase.Maiuscola"). */
  skipNormalize?: boolean;
}) {
  const tc = useThemeColors();
  const normalized = skipNormalize ? (content ?? "") : normalizeContent(content ?? "");

  return (
    <div style={{ lineHeight: 1.7, fontSize: 13.5 }}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          p: ({ children }) => (
            <p style={{ margin: "10px 0", lineHeight: 1.75 }}>{children}</p>
          ),
          strong: ({ children }) => <strong>{children}</strong>,
          em: ({ children }) => <em>{children}</em>,
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
            // Inline code
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
          pre: (({ children }: { children?: React.ReactNode }) => (
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
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          )) as any,
          h1: ({ children }) => (
            <div style={{ fontWeight: 700, fontSize: 18, color: tc.text, margin: "18px 0 8px", borderBottom: `1px solid ${tc.border}`, paddingBottom: 4 }}>
              {children}
            </div>
          ),
          h2: ({ children }) => (
            <div style={{ fontWeight: 700, fontSize: 16, color: tc.text, margin: "16px 0 6px", borderBottom: `1px solid ${tc.border}`, paddingBottom: 3 }}>
              {children}
            </div>
          ),
          h3: ({ children }) => (
            <div style={{ fontWeight: 700, fontSize: 14, color: tc.text, margin: "14px 0 6px" }}>
              {children}
            </div>
          ),
          h4: ({ children }) => (
            <div style={{ fontWeight: 600, fontSize: 13, color: tc.text, margin: "12px 0 4px" }}>
              {children}
            </div>
          ),
          ul: ({ children }) => (
            <ul style={{ margin: "8px 0", paddingLeft: 20 }}>{children}</ul>
          ),
          ol: ({ children }) => (
            <ol style={{ margin: "8px 0", paddingLeft: 22 }}>{children}</ol>
          ),
          li: ({ children }) => (
            <li style={{ marginBottom: 4, lineHeight: 1.65 }}>{children}</li>
          ),
          blockquote: ({ children }) => (
            <blockquote style={{ borderLeft: `3px solid ${tc.accent}`, paddingLeft: 12, margin: "10px 0", color: tc.textSecondary, fontStyle: "italic" }}>
              {children}
            </blockquote>
          ),
          hr: () => (
            <hr style={{ border: "none", borderTop: `1px solid ${tc.border}`, margin: "14px 0" }} />
          ),
          table: ({ children }) => (
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
        }}
      >
        {normalized}
      </ReactMarkdown>
    </div>
  );
}
