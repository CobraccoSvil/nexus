"use client";

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type MouseEvent,
} from "react";
import { useThemeColors } from "../../lib/theme";
import { TruncatedText } from "../truncated-text";
import type { ChatSessionSummary } from "../../lib/api-client";
import type { AgentActivityMap } from "../../lib/use-multi-chat";

function sessionColor(id: string): string {
  const COLORS = [
    "#4A9EFF", "#FF6B6B", "#51CF66", "#FAB005",
    "#CC5DE8", "#20C997", "#FF8787", "#74C0FC",
  ];
  let hash = 0;
  for (let i = 0; i < id.length; i++) hash = (hash * 31 + id.charCodeAt(i)) >>> 0;
  return COLORS[hash % COLORS.length];
}

interface ContextMenu { sessionId: string; x: number; y: number; }

interface SessionTabBarProps {
  allSessions: ChatSessionSummary[];
  openTabs: string[];
  activeTabId: string | null;
  agentActivity: AgentActivityMap;
  onOpenTab: (id: string) => void;
  onCloseTab: (id: string) => void;
  onSetActive: (id: string) => void;
  onNew: () => void;
  onRename: (id: string, title: string) => Promise<void>;
  onDelete: (id: string) => Promise<void>;
  onCompact: (id: string) => Promise<void>;
}

export function SessionTabBar({
  allSessions,
  openTabs,
  activeTabId,
  agentActivity,
  onOpenTab,
  onCloseTab,
  onSetActive,
  onNew,
  onRename,
  onDelete,
  onCompact,
}: SessionTabBarProps) {
  const tc = useThemeColors();
  const [contextMenu, setContextMenu] = useState<ContextMenu | null>(null);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const [compacting, setCompacting] = useState<string | null>(null);
  const contextRef = useRef<HTMLDivElement>(null);
  const renameInputRef = useRef<HTMLInputElement>(null);

  const sessionMap = new Map(allSessions.map((s) => [s.id, s]));

  useEffect(() => {
    if (!contextMenu) return;
    const handler = (e: globalThis.MouseEvent) => {
      if (contextRef.current && !contextRef.current.contains(e.target as Node))
        setContextMenu(null);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [contextMenu]);

  useEffect(() => {
    if (renamingId) setTimeout(() => renameInputRef.current?.select(), 0);
  }, [renamingId]);

  const commitRename = useCallback(async () => {
    if (!renamingId || !renameValue.trim()) { setRenamingId(null); return; }
    await onRename(renamingId, renameValue.trim());
    setRenamingId(null);
  }, [renamingId, renameValue, onRename]);

  const handleRenameKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Enter") void commitRename();
    if (e.key === "Escape") setRenamingId(null);
  };

  const startRename = (id: string) => {
    setRenameValue(sessionMap.get(id)?.title ?? "");
    setRenamingId(id);
    setContextMenu(null);
  };

  const handleDelete = async (id: string) => {
    setContextMenu(null);
    await onDelete(id);
  };

  const handleCompact = async (id: string) => {
    setContextMenu(null);
    setCompacting(id);
    try { await onCompact(id); } finally { setCompacting(null); }
  };

  return (
    <div className="flex-row relative" style={{
      height: 30,
      alignItems: "stretch",
      background: tc.bgSidebar,
      borderBottom: `1px solid ${tc.border}`,
      flexShrink: 0,
    }}>
      {/* Scrollable tab strip */}
      <div className="flex-1 flex-row overflow-x-auto" style={{
        alignItems: "stretch",
        scrollbarWidth: "none",
        maskImage: "linear-gradient(to right, transparent 0px, black 16px, black calc(100% - 16px), transparent 100%)",
        WebkitMaskImage: "linear-gradient(to right, transparent 0px, black 16px, black calc(100% - 16px), transparent 100%)",
      }}>
        {openTabs.map((tabId) => {
          const session = sessionMap.get(tabId);
          const title = session?.title ?? "...";
          const isActive = tabId === activeTabId;
          const isCompacted = session?.status === "compacted";
          const hasAgent = agentActivity.get(tabId) ?? false;
          const isCompacting = compacting === tabId;
          const color = sessionColor(tabId);

          return (
            <div
              key={tabId}
              onMouseDown={() => onSetActive(tabId)}
              onContextMenu={(e: MouseEvent) => {
                e.preventDefault();
                setContextMenu({ sessionId: tabId, x: e.clientX, y: e.clientY });
              }}
              className="flex-row-gap-5 cursor-pointer flex-shrink-0 whitespace-nowrap"
              style={{
                padding: "0 6px 0 8px",
                maxWidth: 160,
                minWidth: 60,
                background: isActive ? tc.bgCard : "transparent",
                borderRight: `1px solid ${tc.border}`,
                borderBottom: isActive ? `2px solid ${color}` : "2px solid transparent",
                opacity: isCompacted ? 0.65 : 1,
              }}
            >
              {/* Color circle */}
              <span style={{
                width: 7, height: 7, borderRadius: "50%",
                background: color, flexShrink: 0,
              }} />

              {/* Title or rename input */}
              {renamingId === tabId ? (
                <input
                  ref={renameInputRef}
                  value={renameValue}
                  onChange={(e) => setRenameValue(e.target.value)}
                  onBlur={() => void commitRename()}
                  onKeyDown={handleRenameKeyDown}
                  onClick={(e) => e.stopPropagation()}
                  className="text-xs"
                  style={{
                    width: 85,
                    background: tc.bgInput, color: tc.text,
                    border: `1px solid ${tc.accent}`, borderRadius: 3,
                    padding: "1px 4px", outline: "none",
                  }}
                />
              ) : (
                <span
                  onDoubleClick={(e) => { e.stopPropagation(); startRename(tabId); }}
                  title={title}
                  className="text-xs flex-1 overflow-hidden"
                  style={{
                    color: isActive ? tc.text : tc.textMuted,
                    textOverflow: "ellipsis",
                  }}
                >
                  {isCompacting ? "⏳" : isCompacted ? "🔮 " : ""}{title}
                </span>
              )}

              {/* Agent active badge */}
              {hasAgent && !renamingId && (
                <span className="flex-shrink-0" style={{
                  width: 6, height: 6, borderRadius: "50%",
                  background: "#51CF66",
                  animation: "tabPulse 1.5s ease-in-out infinite",
                }} />
              )}

              {/* Close button */}
              <span
                onMouseDown={(e) => { e.stopPropagation(); onCloseTab(tabId); }}
                title="Chiudi tab"
                className="text-base text-muted flex-shrink-0"
                style={{
                  lineHeight: 1,
                  padding: "0 1px", marginLeft: 2,
                }}
              >
                ×
              </span>
            </div>
          );
        })}

        {/* Sessions not in tabs (click to open) */}
        {allSessions
          .filter((s) => !openTabs.includes(s.id))
          .slice(0, 3)
          .map((s) => (
            <div
              key={s.id}
              onMouseDown={() => onOpenTab(s.id)}
              title={`Apri: ${s.title}`}
              className="flex-row-gap-4 cursor-pointer flex-shrink-0 text-xs text-muted"
              style={{
                padding: "0 8px",
                opacity: 0.4,
                borderRight: `1px solid ${tc.border}`,
              }}
            >
              <span style={{
                width: 6, height: 6, borderRadius: "50%",
                background: sessionColor(s.id),
              }} />
              <TruncatedText
                text={s.title}
                maxWidth={70}
                tc={tc}
              />
            </div>
          ))}
      </div>

      {/* New chat button */}
      <div
        onMouseDown={onNew}
        title="Nuova chat"
        style={{
          width: 30, height: "100%", display: "flex",
          alignItems: "center", justifyContent: "center",
          cursor: "pointer", color: tc.textMuted, fontSize: 15,
          borderLeft: `1px solid ${tc.border}`, flexShrink: 0,
        }}
      >
        ＋
      </div>

      {/* Context menu */}
      {contextMenu && (
        <div
          ref={contextRef}
          style={{
            position: "fixed", top: contextMenu.y, left: contextMenu.x,
            background: tc.bgSidebar, border: `1px solid ${tc.border}`,
            borderRadius: 7, boxShadow: "0 4px 20px rgba(0,0,0,0.35)",
            zIndex: 9999, minWidth: 170, padding: "4px 0", overflow: "hidden",
          }}
        >
          {[
            { label: "✏️  Rinomina", action: () => startRename(contextMenu.sessionId) },
            { label: "🔮  Compatta e salva in memoria", action: () => void handleCompact(contextMenu.sessionId) },
            { label: "🗑️  Elimina", action: () => void handleDelete(contextMenu.sessionId), danger: true },
          ].map((item) => (
            <div
              key={item.label}
              onMouseDown={item.action}
              style={{
                padding: "7px 14px", fontSize: 12, cursor: "pointer",
                color: item.danger ? "#FF6B6B" : tc.text,
              }}
            >
              {item.label}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
