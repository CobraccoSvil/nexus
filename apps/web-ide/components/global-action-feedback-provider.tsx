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
import { useProjectStore } from "../lib/project-dispatcher";

type FeedbackTone = "success" | "error" | "info";

type FeedbackContextValue = {
  notifyAction: (message: string, tone?: FeedbackTone) => void;
  /// Operazioni di mutazione (POST/PUT/DELETE) attualmente in volo: il footer
  /// le mostra come messaggio "in corso" finche' non arriva l'esito.
  pendingCount: number;
  pendingLabel: string;
};

const FeedbackContext = createContext<FeedbackContextValue | null>(null);

/// Mappa il tono UI sulla severity del toast store (punto unico di
/// visualizzazione: lo store project-dispatcher, reso al centro del footer).
function severityFromTone(tone: FeedbackTone): "info" | "success" | "warning" | "error" {
  if (tone === "success") return "success";
  if (tone === "error") return "error";
  return "info";
}

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

/// Il messaggio da mostrare in una notifica, o `undefined` per far mostrare al
/// chiamante solo "<label> fallita".
///
/// Qui viveva una tabella di sette regex (`ERROR_PATTERNS`) che deduceva la
/// CLASSE dell'errore cercando "429", "timeout", "MetadataMap" nel testo, piu'
/// un `looksTechnical` che sopprimeva tutto cio' che somigliava al `Debug` di
/// una struttura. Entrambi violavano la regola M — lo stato tecnico si legge dai
/// segnali strutturati alla fonte, non dalla prosa — e sbagliavano in entrambi i
/// versi: un "429" comparso per caso nel body faceva dire "limite di richieste",
/// e un errore vero ma dall'aria tecnica spariva del tutto.
///
/// Ora la frase leggibile arriva dal backend, dal punto unico
/// `nexus-types::error_presentation`, dove status e codice erano ancora vivi.
/// Qui resta una sola decisione, e non e' una classificazione: se il testo e' una
/// STRUTTURA (JSON, HTML) invece che prosa, non e' un messaggio e non va in una
/// notifica.
function humanizeError(raw: string | undefined): string | undefined {
  if (!raw) return undefined;
  const trimmed = raw.trim();
  if (!trimmed) return undefined;

  // Pagina HTML / proxy error: non e' un messaggio.
  if (/^\s*</.test(trimmed) || /<html/i.test(trimmed)) {
    return "Il server non ha risposto correttamente";
  }
  // Payload JSON: e' un dato, non una frase.
  if (/^[[{]/.test(trimmed)) return undefined;

  // Testo leggibile: prima riga, max 160 char (layout, non traduzione).
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
  const [pendingCount, setPendingCount] = useState(0);
  const [pendingLabel, setPendingLabel] = useState("Operazione");
  const pendingRef = useRef(0);

  // L'esito dell'azione confluisce nello store toast (punto unico): il footer
  // lo rende al centro come messaggio non invasivo, con auto-dismiss via TTL.
  const notifyAction = useCallback((message: string, tone: FeedbackTone = "info") => {
    useProjectStore.getState().pushToast(severityFromTone(tone), message);
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
    };
  }, [notifyAction]);

  const api = useMemo<FeedbackContextValue>(
    () => ({ notifyAction, pendingCount, pendingLabel }),
    [notifyAction, pendingCount, pendingLabel],
  );

  // Nessun popup fixed: l'IDE non viene piu' invasa. Pending ed esiti vengono
  // resi al centro del footer da FooterToastCenter (status bar).
  return <FeedbackContext.Provider value={api}>{children}</FeedbackContext.Provider>;
}

export function useActionFeedback() {
  const context = useContext(FeedbackContext);
  if (!context) {
    throw new Error("useActionFeedback must be used inside GlobalActionFeedbackProvider");
  }
  return context;
}

