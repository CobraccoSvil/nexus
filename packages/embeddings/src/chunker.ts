import { randomUUID } from "crypto";
import type { Chunk, ChunkMetadata } from "./types.js";

export interface ChunkerOptions {
  maxChunkSize?: number;   // caratteri (default 1500)
  overlap?: number;        // caratteri di overlap tra chunk (default 200)
  minChunkSize?: number;   // chunk troppo piccoli vengono fusi (default 100)
}

const DEFAULTS: Required<ChunkerOptions> = {
  maxChunkSize: 1500,
  overlap: 200,
  minChunkSize: 100,
};

// ─── Markdown chunker ────────────────────────────────────────────────────────

function splitMarkdown(text: string, opts: Required<ChunkerOptions>): string[] {
  // Divide per heading (# ## ###) preservando il contesto del heading nel chunk
  const headingRe = /^(#{1,6})\s+.+$/gm;
  const positions: number[] = [];
  let m: RegExpExecArray | null;

  while ((m = headingRe.exec(text)) !== null) {
    positions.push(m.index);
  }
  positions.push(text.length);

  const sections: string[] = [];
  for (let i = 0; i < positions.length - 1; i++) {
    const section = text.slice(positions[i], positions[i + 1]).trim();
    if (section.length > 0) sections.push(section);
  }

  // Sezioni troppo grandi vengono ulteriormente divise per paragrafo
  const chunks: string[] = [];
  for (const section of sections) {
    if (section.length <= opts.maxChunkSize) {
      chunks.push(section);
    } else {
      chunks.push(...splitByParagraph(section, opts));
    }
  }

  return chunks;
}

function splitByParagraph(text: string, opts: Required<ChunkerOptions>): string[] {
  const paragraphs = text.split(/\n\n+/);
  const chunks: string[] = [];
  let current = "";

  for (const para of paragraphs) {
    if ((current + "\n\n" + para).length > opts.maxChunkSize && current.length > 0) {
      chunks.push(current.trim());
      // Overlap: includi la fine del chunk precedente
      const overlapText = current.slice(-opts.overlap);
      current = overlapText + "\n\n" + para;
    } else {
      current = current ? current + "\n\n" + para : para;
    }
  }
  if (current.trim().length >= opts.minChunkSize) {
    chunks.push(current.trim());
  }
  return chunks;
}

// ─── Code chunker ────────────────────────────────────────────────────────────
// Fase 4: regex-based. Fase futura: tree-sitter AST per parsing preciso.

const FUNCTION_RE =
  /(?:^|\n)(?:export\s+)?(?:async\s+)?(?:function\s+\w+|(?:const|let|var)\s+\w+\s*=\s*(?:async\s*)?\(|class\s+\w+|def\s+\w+\s*\()/g;

function splitCode(text: string, opts: Required<ChunkerOptions>): string[] {
  const positions: number[] = [0];
  let m: RegExpExecArray | null;
  const re = new RegExp(FUNCTION_RE.source, "gm");

  while ((m = re.exec(text)) !== null) {
    if (m.index > 0) positions.push(m.index);
  }
  positions.push(text.length);

  const chunks: string[] = [];
  for (let i = 0; i < positions.length - 1; i++) {
    const chunk = text.slice(positions[i], positions[i + 1]).trim();
    if (chunk.length < opts.minChunkSize) continue;
    if (chunk.length <= opts.maxChunkSize) {
      chunks.push(chunk);
    } else {
      // Chunk troppo grande: dividi per linea con overlap
      chunks.push(...splitByLines(chunk, opts));
    }
  }
  return chunks;
}

function splitByLines(text: string, opts: Required<ChunkerOptions>): string[] {
  const lines = text.split("\n");
  const chunks: string[] = [];
  let current = "";

  for (const line of lines) {
    if ((current + "\n" + line).length > opts.maxChunkSize && current.length > 0) {
      chunks.push(current);
      const overlapLines = current.split("\n").slice(-5).join("\n");
      current = overlapLines + "\n" + line;
    } else {
      current = current ? current + "\n" + line : line;
    }
  }
  if (current.trim().length >= opts.minChunkSize) chunks.push(current.trim());
  return chunks;
}

// ─── Chunker principale ──────────────────────────────────────────────────────

function detectSourceType(filename: string): ChunkMetadata["source_type"] {
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  if (["md", "mdx"].includes(ext)) return "markdown";
  if (["ts", "tsx", "js", "jsx", "py", "go", "rs", "java", "c", "cpp"].includes(ext)) return "code";
  if (["docx", "doc"].includes(ext)) return "docx";
  return "text";
}

function detectLanguage(filename: string): string | undefined {
  const extMap: Record<string, string> = {
    ts: "typescript", tsx: "typescript", js: "javascript", jsx: "javascript",
    py: "python", go: "go", rs: "rust", java: "java", c: "c", cpp: "cpp",
  };
  const ext = filename.split(".").pop()?.toLowerCase() ?? "";
  return extMap[ext];
}

export class Chunker {
  private opts: Required<ChunkerOptions>;

  constructor(opts: ChunkerOptions = {}) {
    this.opts = { ...DEFAULTS, ...opts };
  }

  chunk(params: {
    content: string;
    documentId: string;
    tenantId: string;
    filename?: string;
    sourceType?: ChunkMetadata["source_type"];
    sensitivityTier?: 0 | 1 | 2 | 3;
  }): Chunk[] {
    const sourceType = params.sourceType ?? detectSourceType(params.filename ?? "");
    const language = params.filename ? detectLanguage(params.filename) : undefined;

    const rawChunks =
      sourceType === "markdown"
        ? splitMarkdown(params.content, this.opts)
        : sourceType === "code"
        ? splitCode(params.content, this.opts)
        : splitByParagraph(params.content, this.opts);

    return rawChunks
      .filter((c) => c.trim().length >= this.opts.minChunkSize)
      .map((content, idx) => ({
        id: randomUUID(),
        content,
        metadata: {
          document_id: params.documentId,
          tenant_id: params.tenantId,
          chunk_index: idx,
          total_chunks: rawChunks.length,
          source_type: sourceType,
          language,
          sensitivity_tier: params.sensitivityTier ?? 0,
        },
      }));
  }
}
