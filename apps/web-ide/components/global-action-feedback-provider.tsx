"use client";

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { useThemeColors } from "../lib/theme";
import { TruncatedText } from "./truncated-text";

type FeedbackTone = "success" | "error" | "info";

type FeedbackContextValue = {
  notifyAction: (message: string, tone?: FeedbackTone) => void;
};

const FeedbackContext = createContext<FeedbackContextValue | null>(null);

function inferActionLabel(input: RequestInfo | URL, init?: RequestInit): string {
  const method = (init?.method ?? (input instanceof Request ? input.method : "GET")).toUpperCase();
  const rawUrl =
    typeof input === "string"
      ? input
      : input instanceof URL
        ? input.toString()
        : input.url;

  let pathname = rawUrl.toLowerCase();
  try {
    pathname = new URL(rawUrl, typeof window !== "undefined" ? window.location.origin : "http://localhost").pathname.toLowerCase();
  } catch {
    // keep raw value fallback
  }

  if (pathname.includes("/git")) return `Operazione Git (${method})`;
  if (pathname.includes("/chat")) return `Operazione chat (${method})`;
  if (pathname.includes("/plugins")) return `Operazione plugin (${method})`;
  if (pathname.includes("/projects")) return `Operazione progetto (${method})`;
  if (pathname.includes("/admin")) return `Operazione admin (${method})`;
  return `Operazione (${method})`;
}

// Mappa frammenti tecnici noti → messaggio umano italiano.
// Aggiungere qui nuovi casi quando emergono.
const ERROR_PATTERNS: Array<{ test: RegExp; human: string }> = [
  { test: /ResourceExhausted|Received message larger than max|message larger than/i, human: "Il provider AI ha rifiutato un payload troppo grande, prova con meno contesto" },
  { test: /Unauthenticated|invalid api key|401/i, human: "Credenziali del provider AI non valide" },
  { test: /DeadlineExceeded|timed? ?out|timeout/i, human: "Il provider AI non ha risposto in tempo" },
  { test: /Unavailable|connection refused|ECONNREFUSED|503/i, human: "Servizio momentaneamente non disponibile" },
  { test: /rate ?limit|429|quota/i, human: "Limite di richieste raggiunto, riprova tra poco" },
  { test: /not ?found|404/i, human: "Risorsa non trovata" },
  { test: /forbidden|403/i, human: "Operazione non permessa" },
];

function humanizeError(raw: string | undefined): string | undefined {
  if (!raw) return undefined;
  const trimmed = raw.trim();
  if (!trimmed) return undefined;

  // Pagina HTML / proxy error → generico
  if (/^\s*</.test(trimmed) || /<html/i.test(trimmed)) {
    return "Il server non ha risposto correttamente";
  }

  // Pattern noti
  for (const { test, human } of ERROR_PATTERNS) {
    if (test.test(trimmed)) return human;
  }

  // Contiene roba tecnica grezza (gRPC status, MetadataMap, JSON, stack trace) → sopprimi
  const looksTechnical =
    /MetadataMap|status:\s*\w+|details:\s*\[|grpc[\s_-]?status|stack:|\bat\s+\w+\s*\(/i.test(trimmed) ||
    /^[[{]/.test(trimmed);
  if (looksTechnical) {
    return undefined; // il chiamante mostrerà solo "<label> fallita"
  }

  // Testo leggibile: prima riga, max 160 char
  const line = trimmed.replace(/\r\n/g, "\n").split("\n").find((l) => l.trim().length > 0)?.trim();
  if (!line) return undefined;
  return line.length > 160 ? `${line.slice(0, 157)}...` : line;
}

async function parseResponseError(response: Response): Promise<string | undefined> {
  try {
    const clone = response.clone();
    const contentType = clone.headers.get("content-type")?.toLowerCase() ?? "";
    if (contentType.includes("application/json")) {
      const payload = await clone.json().catch(() => null);
      if (payload && typeof payload === "object") {
        const maybeError =
          typeof (payload as { error?: unknown }).error === "string"
            ? (payload as { error: string }).error
            : typeof (payload as { message?: unknown }).message === "string"
              ? (payload as { message: string }).message
              : undefined;
        if (maybeError) return humanizeError(maybeError);
      }
    }
    const text = await clone.text().catch(() => "");
    return humanizeError(text);
  } catch {
    return undefined;
  }
}

export function GlobalActionFeedbackProvider({ children }: { children: ReactNode }) {
  const tc = useThemeColors();
  const [pendingCount, setPendingCount] = useState(0);
  const [pendingLabel, setPendingLabel] = useState("Operazione");
  const [feedback, setFeedback] = useState<{ tone: FeedbackTone; message: string } | null>(null);
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pendingRef = useRef(0);

  const notifyAction = useCallback((message: string, tone: FeedbackTone = "info") => {
    setFeedback({ tone, message });
    if (timeoutRef.current) {
      clearTimeout(timeoutRef.current);
    }
    const duration = tone === "error" ? 6000 : 2000;
    timeoutRef.current = setTimeout(() => {
      setFeedback(null);
      timeoutRef.current = null;
    }, duration);
  }, []);

  useEffect(() => {
    if (typeof window === "undefined") return;

    const originalFetch = window.fetch.bind(window);
    const wrappedFetch: typeof window.fetch = async (input, init) => {
      const method = (init?.method ?? (input instanceof Request ? input.method : "GET")).toUpperCase();
      const isMutation = method !== "GET" && method !== "HEAD" && method !== "OPTIONS";
      const label = inferActionLabel(input, init);

      if (isMutation) {
        pendingRef.current += 1;
        setPendingCount(pendingRef.current);
        setPendingLabel(label);
      }

      try {
        const response = await originalFetch(input, init);
        if (isMutation) {
          pendingRef.current = Math.max(0, pendingRef.current - 1);
          setPendingCount(pendingRef.current);
          if (response.ok) {
            notifyAction(`${label} completata`, "success");
          } else {
            const details = await parseResponseError(response);
            notifyAction(details ? `${label} fallita: ${details}` : `${label} fallita`, "error");
          }
        }
        return response;
      } catch (error) {
        if (isMutation) {
          pendingRef.current = Math.max(0, pendingRef.current - 1);
          setPendingCount(pendingRef.current);
          const human = error instanceof Error ? humanizeError(error.message) : undefined;
          notifyAction(human ? `${label} fallita: ${human}` : `${label} fallita`, "error");
        }
        throw error;
      }
    };

    window.fetch = wrappedFetch;
    return () => {
      window.fetch = originalFetch;
      if (timeoutRef.current) {
        clearTimeout(timeoutRef.current);
      }
    };
  }, [notifyAction]);

  const api = useMemo<FeedbackContextValue>(() => ({ notifyAction }), [notifyAction]);

  const toneColors =
    feedback?.tone === "error"
      ? { border: tc.error, text: tc.error, background: `${tc.error}14` }
      : feedback?.tone === "success"
        ? { border: "#22c55e", text: "#16a34a", background: "#22c55e12" }
        : { border: tc.border, text: tc.textSecondary, background: tc.bgCard };

  return (
    <FeedbackContext.Provider value={api}>
      {children}
      {pendingCount > 0 && (
        <div
          style={{
            position: "fixed",
            top: 58,
            right: 12,
            zIndex: 1300,
            borderRadius: 999,
            border: `1px solid ${tc.accent}`,
            background: tc.accentBg,
            color: tc.accent,
            fontSize: 12,
            fontWeight: 600,
            padding: "6px 10px",
            display: "inline-flex",
            alignItems: "center",
            gap: 8,
            maxWidth: 360,
            boxShadow: "0 10px 20px rgba(0,0,0,0.18)",
          }}
          aria-live="polite"
        >
          <span>{pendingCount > 1 ? "⏳" : "⌛"}</span>
          <TruncatedText
            text={pendingCount > 1 ? `${pendingCount} operazioni in corso` : `${pendingLabel} in corso`}
            tc={tc}
            maxWidth={200}
          />
        </div>
      )}
      {feedback && (
        <div
          style={{
            position: "fixed",
            right: 12,
            bottom: 36,
            zIndex: 1300,
            borderRadius: 8,
            border: `1px solid ${toneColors.border}`,
            background: toneColors.background,
            color: toneColors.text,
            fontSize: 12,
            padding: "8px 10px",
            maxWidth: 480,
            boxShadow: "0 10px 20px rgba(0,0,0,0.16)",
            whiteSpace: "pre-wrap",
            wordBreak: "break-word",
            lineHeight: 1.4,
          }}
          aria-live="polite"
        >
          <div style={{ display: "flex", alignItems: "flex-start", gap: 8 }}>
            <span style={{ flex: 1 }}>{feedback.message}</span>
            <button
              onClick={() => {
                setFeedback(null);
                if (timeoutRef.current) { clearTimeout(timeoutRef.current); timeoutRef.current = null; }
              }}
              style={{
                background: "none",
                border: "none",
                color: toneColors.text,
                cursor: "pointer",
                padding: 0,
                fontSize: 14,
                lineHeight: 1,
                flexShrink: 0,
                opacity: 0.7,
              }}
              title="Chiudi"
            >
              ✕
            </button>
          </div>
        </div>
      )}
    </FeedbackContext.Provider>
  );
}

export function useActionFeedback() {
  const context = useContext(FeedbackContext);
  if (!context) {
    throw new Error("useActionFeedback must be used inside GlobalActionFeedbackProvider");
  }
  return context;
}

