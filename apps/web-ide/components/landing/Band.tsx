"use client";

import type { ReactNode, CSSProperties } from "react";

const C = {
  dark: { bg: "#06090f", text: "#e2e8f0", muted: "#8494a7" },
  light: { bg: "#fafaf9", text: "#171717", muted: "#737373" },
};

interface BandProps {
  tone: "dark" | "light";
  children: ReactNode;
  id?: string;
  className?: string;
  style?: CSSProperties;
}

export function Band({ tone, children, id, className, style }: BandProps) {
  const palette = C[tone];
  return (
    <section
      id={id}
      className={className}
      style={{
        background: palette.bg,
        color: palette.text,
        padding: "80px 0",
        ...style,
      }}
    >
      <div style={{ maxWidth: 1200, margin: "0 auto", padding: "0 24px" }}>
        {children}
      </div>
    </section>
  );
}

export { C as PALETTE };
