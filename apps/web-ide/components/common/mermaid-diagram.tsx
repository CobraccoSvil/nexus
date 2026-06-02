"use client";

// W3 code-wiki: renderer dei diagrammi Mermaid embeddati nelle note (code_doc).
// Import dinamico di mermaid (libreria pesante) per non gravare sul bundle
// iniziale: viene caricata solo quando una nota contiene un diagramma.

import { useEffect, useRef, useState } from "react";

let _mermaidInitialized = false;

export function MermaidDiagram({ code }: { code: string }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [svg, setSvg] = useState<string>("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const mermaid = (await import("mermaid")).default;
        if (!_mermaidInitialized) {
          mermaid.initialize({
            startOnLoad: false,
            theme: "neutral",
            securityLevel: "strict",
            fontFamily: "inherit",
          });
          _mermaidInitialized = true;
        }
        // id univoco: Math.random e' ammesso lato browser (non e' un workflow).
        const id = "mmd-" + Math.random().toString(36).slice(2);
        const out = await mermaid.render(id, code);
        if (!cancelled) {
          setSvg(out.svg);
          setError(null);
        }
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [code]);

  if (error) {
    // Fallback: mostra il sorgente del diagramma se il render fallisce.
    return (
      <pre
        style={{
          background: "#f5f5f5",
          border: "1px solid #e5e5e5",
          borderRadius: 6,
          padding: 10,
          fontSize: 12,
          overflowX: "auto",
        }}
      >
        {code}
      </pre>
    );
  }

  return (
    <div
      ref={containerRef}
      style={{ display: "flex", justifyContent: "center", padding: "8px 0", overflowX: "auto" }}
      // L'SVG e' sanitizzato da mermaid (securityLevel: strict).
      dangerouslySetInnerHTML={{ __html: svg }}
    />
  );
}
