import type { NextConfig } from "next";

const isDev = process.env.NODE_ENV !== "production";

const nextConfig: NextConfig = {
  // Imposta la root del workspace per evitare warning su lockfile multipli
  // (il progetto redemptor ha un package-lock.json separato non rilevante)
  outputFileTracingRoot: "/home/administrator/ideai",
  typedRoutes: true,
  // Build ID univoco per invalidare la cache del browser ad ogni deploy
  generateBuildId: async () => `build-${Date.now()}`,
  eslint: {
    // ESLint is run separately via `npm run lint` — skip during builds
    // to avoid issues with pnpm workspace vs local node_modules resolution.
    ignoreDuringBuilds: true,
  },
  async headers() {
    return [
      // In dev: no caching — chunk names don't include content hash, stale cache breaks HMR
      // In prod: static assets are content-hashed → safe to cache forever
      {
        source: "/_next/static/:path*",
        headers: isDev
          ? [{ key: "Cache-Control", value: "no-store" }]
          : [{ key: "Cache-Control", value: "public, max-age=31536000, immutable" }],
      },
      // HTML pages → never cached (prevents stale chunk references after deploy)
      {
        source: "/((?!_next/static).*)",
        headers: [
          { key: "Cache-Control", value: "no-store, must-revalidate" },
          { key: "Pragma", value: "no-cache" },
        ],
      },
    ];
  },
  async rewrites() {
    const backend = process.env.BACKEND_URL || "http://localhost:4000";
    const adminService = process.env.ADMIN_SERVICE_URL || "http://localhost:4010";
    // chatService (4020), docService (4030), billingService (4040), pluginService (4050)
    // non ancora attivi — le loro route cadono nel fallback /api/:path* → backend
    return [
      // Embeddings validate/apply → mcp-core (porta 4000), non admin-service (4010)
      // Le route sono implementate in crates/mcp-core/src/environment.rs
      {
        source: "/api/admin/embeddings/:path*",
        destination: `${backend}/api/admin/embeddings/:path*`,
      },
      // Provider cooldown management → mcp-core (porta 4000): il cooldown vive
      // in memoria di mcp-core + Redis, non in admin-service. Vedi
      // environment::admin_reset_provider_cooldown e provider_cooldown.rs.
      {
        source: "/api/admin/providers/:path*",
        destination: `${backend}/api/admin/providers/:path*`,
      },
      // Project learning + feedback + vector compaction → mcp-core (porta 4000):
      // gli handler vivono in crates/mcp-core/src/chat_learning.rs (le tabelle
      // sono nel DB Nexus, non in admin-service). Pattern identico a embeddings
      // e providers: rewrite specifica PRIMA della generica /api/admin/:path*.
      {
        source: "/api/admin/learning/:path*",
        destination: `${backend}/api/admin/learning/:path*`,
      },
      {
        source: "/api/admin/feedback/:path*",
        destination: `${backend}/api/admin/feedback/:path*`,
      },
      {
        source: "/api/admin/vector/:path*",
        destination: `${backend}/api/admin/vector/:path*`,
      },
      {
        source: "/api/admin/sudo/:path*",
        destination: `${backend}/api/admin/sudo/:path*`,
      },
      {
        source: "/api/admin/:path*",
        destination: `${adminService}/api/admin/:path*`,
      },
      // NOTA: /api/chat/* e /api/profiles/* NON vengono routati al chat-service:
      // il chat-service è ancora uno stub incompleto (send_message non chiama mcp-core).
      // Tutte le route chat e profiles sono già implementate in mcp-core (porta 4000)
      // e vengono gestite dal fallback "/api/:path*" → backend qui sotto.
      //
      // NOTA: /api/plugins/*, /api/mcp-servers/*, /api/documents/*, /api/billing/* NON vengono
      // routati ai rispettivi microservizi (4050, 4030, 4040) perché non ancora attivi.
      // Tutte queste route sono già implementate in mcp-core (porta 4000) e vengono
      // gestite dal fallback "/api/:path*" → backend qui sotto.
      {
        source: "/api/:path*",
        destination: `${backend}/api/:path*`,
      },
      {
        source: "/auth/:path*",
        destination: `${backend}/auth/:path*`,
      },
      // Il brain Python e' stato eliminato: gli endpoint neural sono ora ri-esposti
      // in mcp-core (porta 4000) sotto il prefisso /api/neural/*. Il proxy /neural/*
      // del frontend resta invariato lato client, ma punta al core con quel prefisso.
      {
        source: "/neural/:path*",
        destination: `${backend}/api/neural/:path*`,
      },
      {
        source: "/nexus/:path*",
        destination: `${backend}/nexus/:path*`,
      },
    ];
  },
};

export default nextConfig;
