"use client";

import { useEffect, useState } from "react";
import { useI18n, LOCALE_LABELS, type Locale } from "../lib/i18n";

/* ─── palette ─── */
const C = {
  bg: "#06090f",
  surface: "rgba(12,18,30,0.85)",
  border: "#1a2336",
  accent: "#5ba3e6",
  accentGlow: "rgba(91,163,230,0.25)",
  purple: "rgba(139,92,246,0.35)",
  pink: "rgba(236,72,153,0.3)",
  cyan: "rgba(34,211,238,0.25)",
  text: "#e2e8f0",
  muted: "#8494a7",
};

/* ─── responsive hook ─── */
function useBreakpoint() {
  const [w, setW] = useState(1280);
  useEffect(() => {
    const h = () => setW(window.innerWidth);
    h();
    window.addEventListener("resize", h, { passive: true });
    return () => window.removeEventListener("resize", h);
  }, []);
  return { mobile: w < 640, tablet: w < 1024, w };
}

/* ─── reusable blob ─── */
function Blob({
  color,
  size,
  top,
  left,
  right,
  bottom,
}: {
  color: string;
  size: number;
  top?: number | string;
  left?: number | string;
  right?: number | string;
  bottom?: number | string;
}) {
  return (
    <div
      style={{
        position: "absolute",
        width: size,
        height: size,
        borderRadius: "50%",
        background: `radial-gradient(circle, ${color}, transparent 70%)`,
        filter: "blur(90px)",
        top,
        left,
        right,
        bottom,
        pointerEvents: "none",
        zIndex: 0,
      }}
    />
  );
}

/* ─── feature card ─── */
function FeatureCard({ icon, title, desc }: { icon: string; title: string; desc: string }) {
  return (
    <div
      style={{
        background: C.surface,
        border: `1px solid ${C.border}`,
        borderRadius: 16,
        padding: "32px 28px",
        display: "flex",
        flexDirection: "column",
        gap: 12,
        transition: "border-color 0.2s",
      }}
      onMouseEnter={(e) => (e.currentTarget.style.borderColor = C.accent)}
      onMouseLeave={(e) => (e.currentTarget.style.borderColor = C.border)}
    >
      <div style={{ fontSize: 32 }}>{icon}</div>
      <h3 className="text-2xl font-bold m-0" style={{ color: "#fff" }}>{title}</h3>
      <p className="text-lg leading-relaxed text-muted m-0">{desc}</p>
    </div>
  );
}

/* ─── stat card ─── */
function StatCard({ value, label, color, mobile }: { value: string; label: string; color: string; mobile?: boolean }) {
  return (
    <div style={{ textAlign: "center", flex: mobile ? "0 0 45%" : 1, minWidth: mobile ? 0 : 160 }}>
      <div style={{ fontSize: mobile ? 36 : 48, fontWeight: 800, color, lineHeight: 1 }}>{value}</div>
      <div style={{ fontSize: mobile ? 12 : 14, color: C.muted, marginTop: 8 }}>{label}</div>
    </div>
  );
}

/* ─── use-case card ─── */
function UseCaseCard({ title, desc, icon }: { title: string; desc: string; icon: string }) {
  return (
    <div
      style={{
        flex: "1 1 280px",
        background: C.surface,
        border: `1px solid ${C.border}`,
        borderRadius: 16,
        padding: "36px 28px",
        textAlign: "center",
      }}
    >
      <div style={{ fontSize: 40, marginBottom: 16 }}>{icon}</div>
      <h3 style={{ fontSize: 18, fontWeight: 700, margin: "0 0 8px", color: "#fff" }}>{title}</h3>
      <p className="text-lg text-muted leading-relaxed m-0">{desc}</p>
    </div>
  );
}

/* ═══════════════════════════════════════════ MAIN ═══════════════════════════════════════════ */
export default function LandingPage() {
  const { t, locale, setLocale } = useI18n();
  const [scrollY, setScrollY] = useState(0);
  const [langOpen, setLangOpen] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const { mobile, tablet } = useBreakpoint();

  useEffect(() => {
    const h = () => setScrollY(window.scrollY);
    window.addEventListener("scroll", h, { passive: true });
    return () => window.removeEventListener("scroll", h);
  }, []);

  const navOpacity = Math.min(scrollY / 200, 0.95);

  return (
    <div
      style={{
        minHeight: "100vh",
        background: C.bg,
        color: C.text,
        fontFamily:
          "'Inter', -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif",
        overflowX: "hidden",
      }}
    >
      {/* ─── NAVBAR ─── */}
      <nav
        style={{
          position: "fixed",
          top: 0,
          left: 0,
          right: 0,
          zIndex: 100,
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: mobile ? "0 16px" : "0 48px",
          height: 64,
          background: `rgba(6,9,15,${mobile ? 0.95 : navOpacity})`,
          backdropFilter: "blur(12px)",
          borderBottom: `1px solid ${navOpacity > 0.3 || mobile ? C.border : "transparent"}`,
          transition: "border-color 0.3s",
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 10, flexShrink: 0 }}>
          <div
            style={{
              width: 32,
              height: 32,
              borderRadius: 8,
              background: `linear-gradient(135deg, ${C.accent}, #8b5cf6)`,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              fontWeight: 800,
              fontSize: 16,
              color: "#fff",
            }}
          >
            N
          </div>
          <span style={{ fontSize: 20, fontWeight: 700, color: "#fff" }}>Nexus</span>
        </div>

        {mobile ? (
          /* ── Mobile: hamburger + lang + accedi ── */
          <div className="flex-row-gap-12">
            {/* Language switcher compact */}
            <div style={{ position: "relative" }}>
              <button
                onClick={() => { setLangOpen(!langOpen); setMenuOpen(false); }}
                style={{
                  background: "transparent",
                  border: `1px solid ${C.border}`,
                  borderRadius: 6,
                  color: C.muted,
                  fontSize: 12,
                  padding: "4px 8px",
                  cursor: "pointer",
                }}
              >
                {locale.toUpperCase()}
              </button>
              {langOpen && (
                <div
                  style={{
                    position: "absolute",
                    top: "100%",
                    right: 0,
                    marginTop: 4,
                    background: "rgba(12,18,30,0.98)",
                    border: `1px solid ${C.border}`,
                    borderRadius: 8,
                    overflow: "hidden",
                    minWidth: 110,
                    zIndex: 200,
                  }}
                >
                  {(Object.keys(LOCALE_LABELS) as Locale[]).map((l) => (
                    <button
                      key={l}
                      onClick={() => { setLocale(l); setLangOpen(false); }}
                      style={{
                        display: "block",
                        width: "100%",
                        padding: "8px 14px",
                        background: l === locale ? "rgba(91,163,230,0.12)" : "transparent",
                        border: "none",
                        color: l === locale ? C.accent : C.text,
                        fontSize: 13,
                        textAlign: "left",
                        cursor: "pointer",
                      }}
                    >
                      {LOCALE_LABELS[l]}
                    </button>
                  ))}
                </div>
              )}
            </div>

            <a
              href="/login"
              style={{
                padding: "6px 14px",
                borderRadius: 8,
                background: C.accent,
                color: "#fff",
                fontSize: 13,
                fontWeight: 600,
                textDecoration: "none",
              }}
            >
              {t("landing.navbar.access")}
            </a>

            {/* Hamburger */}
            <button
              onClick={() => { setMenuOpen(!menuOpen); setLangOpen(false); }}
              style={{
                background: "transparent",
                border: "none",
                color: C.text,
                fontSize: 22,
                cursor: "pointer",
                padding: 4,
                lineHeight: 1,
              }}
            >
              {menuOpen ? "✕" : "☰"}
            </button>
          </div>
        ) : (
          /* ── Desktop nav links ── */
          <div style={{ display: "flex", alignItems: "center", gap: 28 }}>
            <a href="#features" className="text-lg font-normal no-underline" style={{ color: C.muted }}>
              {t("landing.features")}
            </a>
            <a href="#impact" className="text-lg font-normal no-underline" style={{ color: C.muted }}>
              {t("landing.benefits")}
            </a>
            <a href="#usecases" className="text-lg font-normal no-underline" style={{ color: C.muted }}>
              {t("landing.usecases")}
            </a>

            {/* Language switcher */}
            <div style={{ position: "relative" }}>
              <button
                onClick={() => setLangOpen(!langOpen)}
                style={{
                  background: "transparent",
                  border: `1px solid ${C.border}`,
                  borderRadius: 6,
                  color: C.muted,
                  fontSize: 13,
                  padding: "6px 12px",
                  cursor: "pointer",
                  display: "flex",
                  alignItems: "center",
                  gap: 6,
                }}
              >
                {locale.toUpperCase()}
                <svg width="10" height="6" viewBox="0 0 10 6" fill="none">
                  <path d="M1 1l4 4 4-4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
                </svg>
              </button>
              {langOpen && (
                <div
                  style={{
                    position: "absolute",
                    top: "100%",
                    right: 0,
                    marginTop: 4,
                    background: "rgba(12,18,30,0.98)",
                    border: `1px solid ${C.border}`,
                    borderRadius: 8,
                    overflow: "hidden",
                    minWidth: 120,
                  }}
                >
                  {(Object.keys(LOCALE_LABELS) as Locale[]).map((l) => (
                    <button
                      key={l}
                      onClick={() => { setLocale(l); setLangOpen(false); }}
                      style={{
                        display: "block",
                        width: "100%",
                        padding: "8px 16px",
                        background: l === locale ? "rgba(91,163,230,0.12)" : "transparent",
                        border: "none",
                        color: l === locale ? C.accent : C.text,
                        fontSize: 13,
                        textAlign: "left",
                        cursor: "pointer",
                      }}
                    >
                      {LOCALE_LABELS[l]}
                    </button>
                  ))}
                </div>
              )}
            </div>

            <a
              href="/login"
              style={{
                padding: "8px 20px",
                borderRadius: 8,
                background: C.accent,
                color: "#fff",
                fontSize: 14,
                fontWeight: 600,
                textDecoration: "none",
                border: "none",
              }}
            >
              {t("landing.navbar.access")}
            </a>
          </div>
        )}
      </nav>

      {/* ── Mobile menu dropdown ── */}
      {mobile && menuOpen && (
        <div
          style={{
            position: "fixed",
            top: 64,
            left: 0,
            right: 0,
            zIndex: 99,
            background: "rgba(6,9,15,0.98)",
            borderBottom: `1px solid ${C.border}`,
            padding: "16px 20px",
            display: "flex",
            flexDirection: "column",
            gap: 16,
            backdropFilter: "blur(12px)",
          }}
        >
          <a href="#features" onClick={() => setMenuOpen(false)} className="text-lg font-normal no-underline" style={{ color: C.text }}>
            {t("landing.features")}
          </a>
          <a href="#impact" onClick={() => setMenuOpen(false)} className="text-lg font-normal no-underline" style={{ color: C.text }}>
            {t("landing.benefits")}
          </a>
          <a href="#usecases" onClick={() => setMenuOpen(false)} className="text-lg font-normal no-underline" style={{ color: C.text }}>
            {t("landing.usecases")}
          </a>
        </div>
      )}

      {/* ─── HERO ─── */}
      <section
        style={{
          position: "relative",
          display: "flex",
          flexDirection: "column",
          alignItems: "center",
          justifyContent: "center",
          textAlign: "center",
          minHeight: mobile ? "auto" : "100vh",
          padding: mobile ? "100px 20px 60px" : "120px 24px 80px",
          overflow: "hidden",
        }}
      >
        <Blob color={C.purple} size={mobile ? 400 : 700} top={-200} right={-150} />
        <Blob color={C.pink} size={mobile ? 300 : 500} top={100} left={-200} />
        <Blob color={C.cyan} size={mobile ? 250 : 400} bottom={-100} right={200} />

        <div style={{ position: "relative", zIndex: 1, maxWidth: 800 }}>
          <div
            className="inline-block rounded-full" style={{ padding: "6px 16px",
              background: "rgba(91,163,230,0.12)",
              border: "1px solid rgba(91,163,230,0.25)",
              fontSize: mobile ? 11 : 13,
              fontWeight: 600,
              color: C.accent,
              marginBottom: mobile ? 16 : 24,
            }}
          >
            {t("landing.badge")}
          </div>
          <h1
            style={{
              fontSize: mobile ? 36 : tablet ? 48 : 64,
              fontWeight: 800,
              lineHeight: 1.1,
              margin: mobile ? "0 0 16px" : "0 0 24px",
              color: "#fff",
              letterSpacing: -1,
            }}
          >
            {t("landing.heroTitle1")}
            <br />
            <span
              style={{
                background: "linear-gradient(135deg, #5ba3e6, #8b5cf6, #ec4899)",
                WebkitBackgroundClip: "text",
                WebkitTextFillColor: "transparent",
              }}
            >
              {t("landing.heroTitle2")}
            </span>
          </h1>
          <p
            style={{
              fontSize: mobile ? 15 : 18,
              lineHeight: 1.7,
              color: C.muted,
              maxWidth: 600,
              margin: mobile ? "0 auto 28px" : "0 auto 40px",
            }}
          >
            {t("landing.heroDesc")}
          </p>
          <div className="flex flex-wrap" style={{ gap: mobile ? 12 : 16, justifyContent: "center" }}>
            <a
              href="/login"
              style={{
                padding: mobile ? "12px 24px" : "14px 32px",
                borderRadius: 10,
                background: `linear-gradient(135deg, ${C.accent}, #8b5cf6)`,
                color: "#fff",
                fontSize: mobile ? 14 : 16,
                fontWeight: 700,
                textDecoration: "none",
                boxShadow: `0 0 30px ${C.accentGlow}`,
              }}
            >
              {t("landing.cta")}
            </a>
            <a
              href="#features"
              style={{
                padding: mobile ? "12px 24px" : "14px 32px",
                borderRadius: 10,
                background: "transparent",
                color: C.text,
                fontSize: mobile ? 14 : 16,
                fontWeight: 600,
                textDecoration: "none",
                border: `1px solid ${C.border}`,
              }}
            >
              {t("landing.learnMore")}
            </a>
          </div>
        </div>
      </section>

      {/* ─── FEATURES ─── */}
      <section
        id="features"
        style={{
          position: "relative",
          padding: mobile ? "60px 20px" : "100px 48px",
          maxWidth: 1200,
          margin: "0 auto",
        }}
      >
        <div className="text-center" style={{ marginBottom: mobile ? 36 : 64 }}>
          <h2 className="font-bold mb-4" style={{ fontSize: mobile ? 28 : 40, color: "#fff" }}>
            {t("landing.featuresTitle")}
          </h2>
          <p className="text-muted mx-auto" style={{ fontSize: mobile ? 14 : 16, maxWidth: 560 }}>
            {t("landing.featuresDesc")}
          </p>
        </div>
        <div
          style={{
            display: "grid",
            gridTemplateColumns: mobile ? "1fr" : tablet ? "repeat(2, 1fr)" : "repeat(3, 1fr)",
            gap: mobile ? 16 : 20,
          }}
        >
          <FeatureCard icon="⚡" title={t("landing.feat.context")} desc={t("landing.feat.contextDesc")} />
          <FeatureCard icon="🤖" title={t("landing.feat.parallel")} desc={t("landing.feat.parallelDesc")} />
          <FeatureCard icon="🔮" title={t("landing.feat.cost")} desc={t("landing.feat.costDesc")} />
          <FeatureCard icon="🔄" title={t("landing.feat.speed")} desc={t("landing.feat.speedDesc")} />
          <FeatureCard icon="🛡️" title={t("landing.feat.terminal")} desc={t("landing.feat.terminalDesc")} />
          <FeatureCard icon="🔗" title={t("landing.feat.github")} desc={t("landing.feat.githubDesc")} />
        </div>
      </section>

      {/* ─── LIVE PREVIEW ─── */}
      <section style={{ position: "relative", padding: mobile ? "40px 20px 60px" : "60px 48px 100px", overflow: "hidden" }}>
        <Blob color={C.purple} size={mobile ? 300 : 500} top={-100} left="30%" />
        <Blob color={C.cyan} size={mobile ? 250 : 400} bottom={-150} right="20%" />

        <div style={{ textAlign: "center", marginBottom: mobile ? 24 : 48, position: "relative", zIndex: 1 }}>
          <h2 className="font-bold mb-4" style={{ fontSize: mobile ? 28 : 40, color: "#fff" }}>
            {t("landing.liveTitle")}
          </h2>
          <p style={{ fontSize: mobile ? 14 : 16, color: C.muted }}>
            {t("landing.liveDesc")}
          </p>
        </div>
        <div
          style={{
            position: "relative",
            zIndex: 1,
            maxWidth: 960,
            margin: "0 auto",
            borderRadius: mobile ? 12 : 16,
            border: `1px solid ${C.border}`,
            background: "rgba(8,17,29,0.9)",
            overflow: "hidden",
            boxShadow: "0 0 60px rgba(91,163,230,0.08)",
          }}
        >
          {/* Mock title bar */}
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: 6,
              padding: mobile ? "8px 12px" : "12px 16px",
              borderBottom: `1px solid ${C.border}`,
              background: "rgba(12,18,30,0.95)",
            }}
          >
            <div style={{ width: 10, height: 10, borderRadius: "50%", background: "#ef4444" }} />
            <div style={{ width: 10, height: 10, borderRadius: "50%", background: "#eab308" }} />
            <div style={{ width: 10, height: 10, borderRadius: "50%", background: "#22c55e" }} />
            <span style={{ fontSize: 11, color: C.muted, marginLeft: 8 }}>Nexus — main.rs</span>
          </div>

          {mobile ? (
            /* ── Mobile: stacked layout ── */
            <div style={{ fontSize: 12 }}>
              {/* Code */}
              <div style={{ padding: 12, fontFamily: "'JetBrains Mono', monospace", fontSize: 11, borderBottom: `1px solid ${C.border}` }}>
                <div><span style={{ color: "#c678dd" }}>fn</span> <span style={{ color: "#61dafb" }}>main</span>() {"{"}</div>
                <div style={{ paddingLeft: 12 }}><span style={{ color: "#c678dd" }}>let</span> srv = <span style={{ color: "#61dafb" }}>NexusEngine</span>::<span style={{ color: "#e5c07b" }}>new</span>();</div>
                <div style={{ paddingLeft: 12 }}>srv.<span style={{ color: "#e5c07b" }}>orchestrate</span>();</div>
                <div style={{ paddingLeft: 12 }}>srv.<span style={{ color: "#e5c07b" }}>spawn_agents</span>(<span style={{ color: "#d19a66" }}>5</span>);</div>
                <div>{"}"}</div>
              </div>
              {/* Chat */}
              <div style={{ padding: 12, display: "flex", flexDirection: "column", gap: 8 }}>
                <div style={{ color: C.accent, fontWeight: 600, fontSize: 11 }}>AI ASSISTANT</div>
                <div style={{ background: "rgba(91,163,230,0.08)", borderRadius: 8, padding: "8px 10px", color: C.text, fontSize: 11, lineHeight: 1.5 }}>
                  {t("landing.mockChat1")}
                </div>
                <div style={{ background: "rgba(139,92,246,0.1)", borderRadius: 8, padding: "8px 10px", color: C.text, fontSize: 11, lineHeight: 1.5 }}>
                  {t("landing.mockChat2")}
                </div>
              </div>
            </div>
          ) : (
            /* ── Desktop: 3 columns ── */
            <div style={{ display: "flex", height: 320 }}>
              <div
                style={{
                  width: 200,
                  borderRight: `1px solid ${C.border}`,
                  padding: "16px 12px",
                  fontSize: 13,
                  color: C.muted,
                  display: "flex",
                  flexDirection: "column",
                  gap: 6,
                }}
              >
                <div style={{ color: C.accent, fontWeight: 600, marginBottom: 4 }}>EXPLORER</div>
                <div>📁 src/</div>
                <div style={{ paddingLeft: 16 }}>📄 main.rs</div>
                <div style={{ paddingLeft: 16 }}>📄 lib.rs</div>
                <div style={{ paddingLeft: 16 }}>📁 handlers/</div>
                <div>📁 tests/</div>
                <div>📄 Cargo.toml</div>
              </div>
              <div style={{ flex: 1, padding: 16, fontSize: 13, fontFamily: "'JetBrains Mono', monospace" }}>
                <div><span style={{ color: "#c678dd" }}>fn</span> <span style={{ color: "#61dafb" }}>main</span>() {"{"}</div>
                <div style={{ paddingLeft: 20 }}><span style={{ color: "#c678dd" }}>let</span> server = <span style={{ color: "#61dafb" }}>NexusEngine</span>::<span style={{ color: "#e5c07b" }}>new</span>();</div>
                <div style={{ paddingLeft: 20 }}>server.<span style={{ color: "#e5c07b" }}>orchestrate</span>();</div>
                <div style={{ paddingLeft: 20 }}><span style={{ color: "#7f848e" }}>// AI agents running in parallel</span></div>
                <div style={{ paddingLeft: 20 }}>server.<span style={{ color: "#e5c07b" }}>spawn_agents</span>(<span style={{ color: "#d19a66" }}>5</span>);</div>
                <div>{"}"}</div>
              </div>
              <div
                style={{
                  width: 260,
                  borderLeft: `1px solid ${C.border}`,
                  padding: 16,
                  fontSize: 12,
                  display: "flex",
                  flexDirection: "column",
                  gap: 10,
                }}
              >
                <div style={{ color: C.accent, fontWeight: 600, fontSize: 13 }}>AI ASSISTANT</div>
                <div style={{ background: "rgba(91,163,230,0.08)", borderRadius: 8, padding: "10px 12px", color: C.text, lineHeight: 1.5 }}>
                  {t("landing.mockChat1")}
                </div>
                <div style={{ background: "rgba(139,92,246,0.1)", borderRadius: 8, padding: "10px 12px", color: C.text, lineHeight: 1.5 }}>
                  {t("landing.mockChat2")}
                </div>
              </div>
            </div>
          )}
        </div>
      </section>

      {/* ─── STATS / IMPACT ─── */}
      <section
        id="impact"
        style={{
          position: "relative",
          padding: mobile ? "60px 20px" : "100px 48px",
          overflow: "hidden",
        }}
      >
        <Blob color={C.pink} size={mobile ? 350 : 600} top={-200} left="50%" />

        <div style={{ textAlign: "center", marginBottom: mobile ? 36 : 64, position: "relative", zIndex: 1 }}>
          <h2 className="font-bold mb-4" style={{ fontSize: mobile ? 28 : 40, color: "#fff" }}>
            {t("landing.statsTitle")}
          </h2>
          <p style={{ fontSize: mobile ? 14 : 16, color: C.muted }}>
            {t("landing.statsDesc")}
          </p>
        </div>
        <div
          style={{
            display: "flex",
            flexWrap: "wrap",
            gap: mobile ? 24 : 40,
            justifyContent: "center",
            position: "relative",
            zIndex: 1,
            maxWidth: 1000,
            margin: "0 auto",
          }}
        >
          <StatCard value="<1ms" label={t("landing.stat.speed")} color={C.accent} mobile={mobile} />
          <StatCard value="60" label={t("landing.stat.cost")} color="#8b5cf6" mobile={mobile} />
          <StatCard value="314" label={t("landing.stat.agents")} color="#ec4899" mobile={mobile} />
          <StatCard value="12" label={t("landing.stat.context")} color="#22d3ee" mobile={mobile} />
        </div>

        <div
          style={{
            position: "absolute",
            bottom: 0,
            left: 0,
            right: 0,
            height: 120,
            background:
              "linear-gradient(180deg, transparent, rgba(91,163,230,0.05) 40%, rgba(139,92,246,0.08) 70%, rgba(236,72,153,0.06))",
            pointerEvents: "none",
          }}
        />
      </section>

      {/* ─── USE CASES ─── */}
      <section
        id="usecases"
        style={{
          padding: mobile ? "60px 20px" : "80px 48px 100px",
          maxWidth: 1200,
          margin: "0 auto",
        }}
      >
        <div className="text-center" style={{ marginBottom: mobile ? 36 : 64 }}>
          <h2 className="font-bold mb-4" style={{ fontSize: mobile ? 28 : 40, color: "#fff" }}>
            {t("landing.usecasesTitle")}
          </h2>
          <p style={{ fontSize: mobile ? 14 : 16, color: C.muted }}>
            {t("landing.usecasesDesc")}
          </p>
        </div>
        <div style={{ display: "flex", gap: mobile ? 16 : 20, flexWrap: "wrap", justifyContent: "center" }}>
          <UseCaseCard icon="👨‍💻" title={t("landing.uc.solo")} desc={t("landing.uc.soloDesc")} />
          <UseCaseCard icon="👥" title={t("landing.uc.team")} desc={t("landing.uc.teamDesc")} />
          <UseCaseCard icon="🏢" title={t("landing.uc.enterprise")} desc={t("landing.uc.enterpriseDesc")} />
        </div>
      </section>

      {/* ─── FOOTER CTA ─── */}
      <section
        style={{
          position: "relative",
          padding: mobile ? "60px 20px 40px" : "100px 48px 60px",
          textAlign: "center",
          overflow: "hidden",
        }}
      >
        <Blob color={C.purple} size={mobile ? 300 : 500} top={-150} left="20%" />
        <Blob color={C.cyan} size={mobile ? 250 : 400} top={-100} right="15%" />

        <div style={{ position: "relative", zIndex: 1 }}>
          <h2 className="font-bold mb-4" style={{ fontSize: mobile ? 32 : 44, color: "#fff" }}>
            {t("landing.ctaTitle")}
          </h2>
          <p style={{ fontSize: mobile ? 14 : 16, color: C.muted, marginBottom: mobile ? 24 : 36 }}>
            {t("landing.ctaDesc")}
          </p>
          <a
            href="/login"
            style={{
              display: "inline-block",
              padding: mobile ? "14px 32px" : "16px 40px",
              borderRadius: 10,
              background: `linear-gradient(135deg, ${C.accent}, #8b5cf6)`,
              color: "#fff",
              fontSize: mobile ? 15 : 17,
              fontWeight: 700,
              textDecoration: "none",
              boxShadow: `0 0 40px ${C.accentGlow}`,
            }}
          >
            {t("landing.ctaButton")}
          </a>
        </div>


        {/* New Features: Step 5-8 */}
        <section
          style={{
            position: "relative",
            padding: mobile ? "60px 24px 80px" : "100px 48px",
            maxWidth: 1200,
            margin: "0 auto",
            textAlign: "center",
          }}
        >
          <div className="absolute rounded-full" style={{ width: 400, height: 400, background: `radial-gradient(circle, rgba(100,200,150,0.2), transparent 70%)`, filter: "blur(90px)", top: -100, right: "10%", pointerEvents: "none" }}></div>
          
          <div style={{ position: "relative", zIndex: 1 }}>
            <div className="inline-block rounded-full" style={{ padding: "6px 16px", background: "rgba(100,200,150,0.12)", border: `1px solid rgba(100,200,150,0.25)`, fontSize: 13, fontWeight: 600, color: "#64c896", marginBottom: 24 }}>
              ✨ Step 5-8: Advanced Tool Selection
            </div>
            
            <h2 style={{ fontSize: mobile ? 32 : 40, fontWeight: 800, color: "#fff", margin: "0 0 24px", lineHeight: 1.2 }}>
              Smarter Tool Selection.<br/>
              <span style={{ background: `linear-gradient(135deg, #64c896, #22d3ee)`, WebkitBackgroundClip: "text", WebkitTextFillColor: "transparent" }}>Less Context, More Precision.</span>
            </h2>
            
            <p style={{ fontSize: mobile ? 14 : 16, color: C.muted, maxWidth: 600, margin: "0 auto 40px", lineHeight: 1.7 }}>
              ONNX 384-dim embeddings + CachedEmbedder for semantic tool selection. Confidence scoring, analytics, and batch assignment — all production-ready.
            </p>
            
            <div style={{ display: "grid", gridTemplateColumns: mobile ? "1fr" : "repeat(2, 1fr)", gap: 20, marginTop: 32 }}>
              <div style={{ background: C.surface, border: `1px solid ${C.border}`, borderRadius: 12, padding: 20 }}>
                <div style={{ fontSize: 28, marginBottom: 8 }}>🧠</div>
                <h3 style={{ fontSize: 16, fontWeight: 700, margin: "0 0 8px", color: "#fff" }}>ONNX 384-dim Embeddings</h3>
                <p style={{ fontSize: 13, color: C.muted, margin: 0 }}>3x better semantic quality. MiniLM with AVX2 support, fallback to HashEmbedder.</p>
              </div>
              
              <div style={{ background: C.surface, border: `1px solid ${C.border}`, borderRadius: 12, padding: 20 }}>
                <div style={{ fontSize: 28, marginBottom: 8 }}>💾</div>
                <h3 style={{ fontSize: 16, fontWeight: 700, margin: "0 0 8px", color: "#fff" }}>CachedEmbedder (10k)</h3>
                <p style={{ fontSize: 13, color: C.muted, margin: 0 }}>~1ms cache hits, 60-80% hit rate. RwLock-safe concurrent access.</p>
              </div>
              
              <div style={{ background: C.surface, border: `1px solid ${C.border}`, borderRadius: 12, padding: 20 }}>
                <div style={{ fontSize: 28, marginBottom: 8 }}>📊</div>
                <h3 style={{ fontSize: 16, fontWeight: 700, margin: "0 0 8px", color: "#fff" }}>Confidence Scoring</h3>
                <p style={{ fontSize: 13, color: C.muted, margin: 0 }}>Every tool selection tracked: confidence (0-1), method (Semantic/Keyword/Lazy), analytics.</p>
              </div>
              
              <div style={{ background: C.surface, border: `1px solid ${C.border}`, borderRadius: 12, padding: 20 }}>
                <div style={{ fontSize: 28, marginBottom: 8 }}>🤖</div>
                <h3 style={{ fontSize: 16, fontWeight: 700, margin: "0 0 8px", color: "#fff" }}>Batch Tool Assignment</h3>
                <p style={{ fontSize: 13, color: C.muted, margin: 0 }}>AI classifies tools for 60 templates. POST endpoint, mcp_tools_json saved to DB.</p>
              </div>
            </div>
            
            <div style={{ marginTop: 40, padding: "20px 24px", background: "rgba(100,200,150,0.08)", border: `1px solid rgba(100,200,150,0.2)`, borderRadius: 8 }}>
              <p style={{ fontSize: 13, color: C.muted, margin: 0 }}>
                <strong style={{ color: "#64c896" }}>70% token savings:</strong> Semantic search reduces tool definitions from 31 to 7-9 tools per request.
              </p>
            </div>
          </div>
        </section>

        {/* Footer */}
        <div
          style={{
            marginTop: mobile ? 48 : 80,
            paddingTop: 24,
            borderTop: `1px solid ${C.border}`,
            display: "flex",
            flexDirection: mobile ? "column" : "row",
            justifyContent: "space-between",
            alignItems: "center",
            gap: mobile ? 16 : 0,
            fontSize: 13,
            color: C.muted,
            position: "relative",
            zIndex: 1,
          }}
        >
          <span>&copy; 2026 Nexus. All rights reserved.</span>
          <div style={{ display: "flex", gap: mobile ? 16 : 24, flexWrap: "wrap", justifyContent: "center" }}>
            <a href="#features" style={{ color: C.muted, textDecoration: "none" }}>{t("landing.features")}</a>
            <a href="#impact" style={{ color: C.muted, textDecoration: "none" }}>{t("landing.benefits")}</a>
            <a href="#usecases" style={{ color: C.muted, textDecoration: "none" }}>{t("landing.usecases")}</a>
            <a href="/login" style={{ color: C.muted, textDecoration: "none" }}>{t("landing.login")}</a>
          </div>
        </div>
      </section>
    </div>
  );
}
