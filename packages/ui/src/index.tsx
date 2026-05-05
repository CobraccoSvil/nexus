import type { PropsWithChildren } from "react";

export function PanelCard({
  title,
  subtitle,
  children,
}: PropsWithChildren<{ title: string; subtitle?: string }>) {
  return (
    <section
      style={{
        border: "1px solid #24354d",
        borderRadius: 12,
        background: "#0f1723",
        color: "#e5eef9",
        padding: 16,
      }}
    >
      <header style={{ marginBottom: 12 }}>
        <div style={{ fontSize: 16, fontWeight: 700 }}>{title}</div>
        {subtitle ? (
          <div style={{ fontSize: 13, color: "#8ea1bc", marginTop: 4 }}>{subtitle}</div>
        ) : null}
      </header>
      {children}
    </section>
  );
}

