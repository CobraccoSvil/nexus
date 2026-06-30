"use client";

import { useThemeColors } from "../../lib/theme";

export type DiffLineKind = "meta" | "add" | "remove" | "context";
export interface ParsedDiffLine {
  kind: DiffLineKind;
  content: string;
  hunkIndex: number;
  isHunkHeader: boolean;
  lineNumber: number | null;
}

type HighlightTokenKind = "plain" | "keyword" | "string" | "number" | "comment";
interface HighlightToken {
  text: string;
  kind: HighlightTokenKind;
}

export type LanguageHint = "ts" | "python" | "sql" | "json" | "rust" | "shell" | "plain";

export function parseUnifiedDiff(raw: string): ParsedDiffLine[] {
  if (!raw) return [];
  let currentHunkIndex = -1;
  let currentTargetLine = 1;
  return raw.split(/\r?\n/).map((line) => {
    const isHunkHeader = line.startsWith("@@");
    if (isHunkHeader) {
      currentHunkIndex += 1;
      const match = line.match(/^@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
      currentTargetLine = match ? Number.parseInt(match[1], 10) : 1;
    }
    if (
      line.startsWith("diff --git") ||
      line.startsWith("index ") ||
      line.startsWith("--- ") ||
      line.startsWith("+++ ") ||
      isHunkHeader
    ) {
      return { kind: "meta", content: line, hunkIndex: currentHunkIndex, isHunkHeader, lineNumber: null };
    }
    if (line.startsWith("+")) {
      const lineNumber = currentTargetLine;
      currentTargetLine += 1;
      return { kind: "add", content: line, hunkIndex: currentHunkIndex, isHunkHeader: false, lineNumber };
    }
    if (line.startsWith("-")) {
      const lineNumber = Math.max(1, currentTargetLine);
      return { kind: "remove", content: line, hunkIndex: currentHunkIndex, isHunkHeader: false, lineNumber };
    }
    if (line.length > 0) {
      const lineNumber = currentTargetLine;
      currentTargetLine += 1;
      return { kind: "context", content: line, hunkIndex: currentHunkIndex, isHunkHeader: false, lineNumber };
    }
    return { kind: "context", content: line, hunkIndex: currentHunkIndex, isHunkHeader: false, lineNumber: null };
  });
}

export function lineColors(kind: DiffLineKind, tc: ReturnType<typeof useThemeColors>) {
  if (kind === "add") {
    return { background: "rgba(34, 197, 94, 0.16)", color: tc.text };
  }
  if (kind === "remove") {
    return { background: "rgba(239, 68, 68, 0.16)", color: tc.text };
  }
  if (kind === "meta") {
    return { background: tc.bgCard, color: tc.textSecondary };
  }
  return { background: "transparent", color: tc.text };
}

export function detectLanguageHint(path: string): LanguageHint {
  const lower = path.toLowerCase();
  if (lower.endsWith(".ts") || lower.endsWith(".tsx") || lower.endsWith(".js") || lower.endsWith(".jsx")) return "ts";
  if (lower.endsWith(".py")) return "python";
  if (lower.endsWith(".sql")) return "sql";
  if (lower.endsWith(".json")) return "json";
  if (lower.endsWith(".rs")) return "rust";
  if (lower.endsWith(".sh") || lower.endsWith(".bash")) return "shell";
  return "plain";
}

function keywordRegexFor(language: LanguageHint): RegExp | null {
  if (language === "ts") {
    return /^(?:const|let|var|function|class|interface|type|extends|implements|import|from|export|return|if|else|for|while|switch|case|break|continue|new|async|await|try|catch|finally|throw|public|private|protected|readonly|static)\b/;
  }
  if (language === "python") {
    return /^(?:def|class|import|from|as|return|if|elif|else|for|while|break|continue|try|except|finally|raise|with|lambda|async|await|pass|None|True|False)\b/;
  }
  if (language === "sql") {
    return /^(?:SELECT|FROM|WHERE|JOIN|LEFT|RIGHT|INNER|OUTER|ON|GROUP|BY|ORDER|LIMIT|OFFSET|INSERT|INTO|VALUES|UPDATE|SET|DELETE|CREATE|ALTER|DROP|TABLE|INDEX|AND|OR|NOT|AS|DISTINCT)\b/i;
  }
  if (language === "json") {
    return /^(?:true|false|null)\b/;
  }
  if (language === "rust") {
    return /^(?:fn|let|mut|pub|struct|enum|impl|trait|use|mod|match|if|else|for|while|loop|return|async|await|move|where|Self|self)\b/;
  }
  if (language === "shell") {
    return /^(?:if|then|else|fi|for|in|do|done|case|esac|function|local|export|echo|cd|grep|sed|awk|cat)\b/;
  }
  return null;
}

function commentRegexFor(language: LanguageHint): RegExp | null {
  if (language === "python" || language === "shell") return /^#.*/;
  if (language === "sql") return /^--.*/;
  if (language === "ts" || language === "rust") return /^\/\/.*/;
  return null;
}

function tokenColor(kind: HighlightTokenKind, tc: ReturnType<typeof useThemeColors>) {
  if (kind === "keyword") return "#60a5fa";
  if (kind === "string") return "#f59e0b";
  if (kind === "number") return "#22c55e";
  if (kind === "comment") return tc.textMuted;
  return tc.text;
}

function highlightTokens(input: string, language: LanguageHint): HighlightToken[] {
  if (!input) return [{ text: "", kind: "plain" }];

  const tokens: HighlightToken[] = [];
  let cursor = 0;
  const keywordRegex = keywordRegexFor(language);
  const commentRegex = commentRegexFor(language);
  const numberRegex = /^\d+(?:\.\d+)?\b/;
  const stringRegex = /^"(?:\\.|[^"\\])*"|^'(?:\\.|[^'\\])*'|^`(?:\\.|[^`\\])*`/;

  while (cursor < input.length) {
    const rest = input.slice(cursor);

    const commentMatch = commentRegex ? rest.match(commentRegex) : null;
    if (commentMatch?.index === 0) {
      tokens.push({ text: commentMatch[0], kind: "comment" });
      cursor += commentMatch[0].length;
      continue;
    }

    const stringMatch = rest.match(stringRegex);
    if (stringMatch?.index === 0) {
      tokens.push({ text: stringMatch[0], kind: "string" });
      cursor += stringMatch[0].length;
      continue;
    }

    const numberMatch = rest.match(numberRegex);
    if (numberMatch?.index === 0) {
      tokens.push({ text: numberMatch[0], kind: "number" });
      cursor += numberMatch[0].length;
      continue;
    }

    const keywordMatch = keywordRegex ? rest.match(keywordRegex) : null;
    if (keywordMatch?.index === 0) {
      tokens.push({ text: keywordMatch[0], kind: "keyword" });
      cursor += keywordMatch[0].length;
      continue;
    }

    tokens.push({ text: input[cursor], kind: "plain" });
    cursor += 1;
  }

  return mergePlainTokens(tokens);
}

function mergePlainTokens(tokens: HighlightToken[]) {
  const merged: HighlightToken[] = [];
  for (const token of tokens) {
    const previous = merged[merged.length - 1];
    if (previous && previous.kind === "plain" && token.kind === "plain") {
      previous.text += token.text;
      continue;
    }
    merged.push({ ...token });
  }
  return merged;
}

export function renderSyntaxHighlighted(
  value: string,
  lineKind: DiffLineKind,
  language: LanguageHint,
  tc: ReturnType<typeof useThemeColors>,
) {
  if (lineKind === "meta") {
    return value;
  }

  const hasPrefix = value.startsWith("+") || value.startsWith("-") || value.startsWith(" ");
  const prefix = hasPrefix ? value[0] : "";
  const body = hasPrefix ? value.slice(1) : value;
  const tokens = highlightTokens(body, language);

  return (
    <>
      {prefix}
      {tokens.map((token, index) => (
        <span key={`${index}-${token.kind}-${token.text}`} style={{ color: tokenColor(token.kind, tc) }}>
          {token.text}
        </span>
      ))}
    </>
  );
}

export function renderDiffContent(
  line: ParsedDiffLine,
  language: LanguageHint,
  tc: ReturnType<typeof useThemeColors>,
) {
  if (!line.content) return " ";
  return renderSyntaxHighlighted(line.content, line.kind, language, tc);
}

export function renderUnifiedDiff(
  lines: ParsedDiffLine[],
  tc: ReturnType<typeof useThemeColors>,
  activeHunk: number,
  selectedPath: string,
  onOpenFileAtLine?: (path: string, line: number) => Promise<void>,
) {
  const language = detectLanguageHint(selectedPath);
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 1 }}>
      {lines.map((line, index) => {
        const colors = lineColors(line.kind, tc);
        const focused = activeHunk >= 0 && line.hunkIndex === activeHunk;
        const clickable = !!onOpenFileAtLine && !!line.lineNumber;
        return (
          <div
            key={`${index}-${line.content}`}
            onClick={() => {
              if (clickable && line.lineNumber) {
                void onOpenFileAtLine(selectedPath, line.lineNumber);
              }
            }}
            style={{
              ...colors,
              borderRadius: 4,
              padding: "2px 6px",
              whiteSpace: "pre",
              overflowX: "auto",
              boxShadow: focused ? `inset 0 0 0 1px ${tc.accent}` : "none",
              cursor: clickable ? "pointer" : "default",
            }}
          >
            {renderDiffContent(line, language, tc)}
          </div>
        );
      })}
    </div>
  );
}

function DiffCell({
  line,
  tc,
  align,
  activeHunk,
  language,
  selectedPath,
  onOpenFileAtLine,
}: {
  line?: ParsedDiffLine;
  tc: ReturnType<typeof useThemeColors>;
  align: "left" | "right";
  activeHunk: number;
  language: LanguageHint;
  selectedPath: string;
  onOpenFileAtLine?: (path: string, line: number) => Promise<void>;
}) {
  if (!line) {
    return <div style={{ minHeight: 20 }} />;
  }

  const colors = lineColors(line.kind, tc);
  const normalized =
    align === "left" && line.kind === "add"
      ? ""
      : align === "right" && line.kind === "remove"
        ? ""
        : line.content;
  const focused = activeHunk >= 0 && line.hunkIndex === activeHunk;
  const clickable = !!onOpenFileAtLine && !!line.lineNumber;

  return (
    <div
      onClick={() => {
        if (clickable && line.lineNumber) {
          void onOpenFileAtLine(selectedPath, line.lineNumber);
        }
      }}
      style={{
        ...colors,
        borderRadius: 4,
        padding: "2px 6px",
        whiteSpace: "pre",
        overflowX: "auto",
        boxShadow: focused ? `inset 0 0 0 1px ${tc.accent}` : "none",
        cursor: clickable ? "pointer" : "default",
      }}
    >
      {normalized ? renderSyntaxHighlighted(normalized, line.kind, language, tc) : " "}
    </div>
  );
}

function DiffSplitRow({
  row,
  tc,
  activeHunk,
  language,
  selectedPath,
  onOpenFileAtLine,
}: {
  row: { left?: ParsedDiffLine; right?: ParsedDiffLine };
  tc: ReturnType<typeof useThemeColors>;
  activeHunk: number;
  language: LanguageHint;
  selectedPath: string;
  onOpenFileAtLine?: (path: string, line: number) => Promise<void>;
}) {
  return (
    <>
      <DiffCell
        line={row.left}
        tc={tc}
        align="left"
        activeHunk={activeHunk}
        language={language}
        selectedPath={selectedPath}
        onOpenFileAtLine={onOpenFileAtLine}
      />
      <DiffCell
        line={row.right}
        tc={tc}
        align="right"
        activeHunk={activeHunk}
        language={language}
        selectedPath={selectedPath}
        onOpenFileAtLine={onOpenFileAtLine}
      />
    </>
  );
}

export function renderSplitDiff(
  lines: ParsedDiffLine[],
  tc: ReturnType<typeof useThemeColors>,
  activeHunk: number,
  selectedPath: string,
  onOpenFileAtLine?: (path: string, line: number) => Promise<void>,
) {
  const language = detectLanguageHint(selectedPath);
  const rows: Array<{ left?: ParsedDiffLine; right?: ParsedDiffLine }> = [];

  for (let i = 0; i < lines.length; i += 1) {
    const current = lines[i];
    const next = lines[i + 1];
    if (current.kind === "remove" && next?.kind === "add") {
      rows.push({ left: current, right: next });
      i += 1;
      continue;
    }
    if (current.kind === "remove") {
      rows.push({ left: current });
      continue;
    }
    if (current.kind === "add") {
      rows.push({ right: current });
      continue;
    }
    rows.push({ left: current, right: current });
  }

  return (
    <div
      style={{
        display: "grid",
        gridTemplateColumns: "1fr 1fr",
        gap: 6,
      }}
    >
      {rows.map((row, index) => (
        <DiffSplitRow
          key={index}
          row={row}
          tc={tc}
          activeHunk={activeHunk}
          language={language}
          selectedPath={selectedPath}
          onOpenFileAtLine={onOpenFileAtLine}
        />
      ))}
    </div>
  );
}
