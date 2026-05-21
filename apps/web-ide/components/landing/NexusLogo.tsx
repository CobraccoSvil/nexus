"use client";

/**
 * NexusLogo — replica inline del favicon (`app/icon.svg`) come componente
 * React riutilizzabile. Usato nella NavBar di landing/marketing.
 *
 * Forma: rettangolo arrotondato con N bianca centrata.
 * Default color: stesso del favicon (`#7c3aed`). Il prop `color` permette
 * di personalizzarlo se serve (es. tema dark / pricing page).
 */
export function NexusLogo({
  size = 28,
  color = "#7c3aed",
  ariaLabel = "Nexus",
}: {
  size?: number;
  color?: string;
  ariaLabel?: string;
}) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 32 32"
      width={size}
      height={size}
      role="img"
      aria-label={ariaLabel}
      style={{ display: "block", flexShrink: 0 }}
    >
      <rect width="32" height="32" rx="6" fill={color} />
      <text
        x="16"
        y="23"
        textAnchor="middle"
        fontFamily="monospace"
        fontSize="20"
        fontWeight="bold"
        fill="white"
      >
        N
      </text>
    </svg>
  );
}
