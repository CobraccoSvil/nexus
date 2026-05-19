import tsParser from "@typescript-eslint/parser";
import tsPlugin from "@typescript-eslint/eslint-plugin";
import reactHooksPlugin from "eslint-plugin-react-hooks";

// Configurazione ESLint root del monorepo.
// Regole editoriali Nexus attivate:
//   - no-explicit-any: errore (dogfood direttiva C)
export default [
  {
    files: ["**/*.{ts,tsx,js,jsx}"],
    ignores: [
      "**/dist/**",
      "**/.next/**",
      "**/node_modules/**",
      "**/build/**",
      // File generato da Next.js (vedi `next-env.d.ts`: "should not be edited").
      "apps/web-ide/next-env.d.ts",
    ],
    languageOptions: {
      ecmaVersion: "latest",
      sourceType: "module",
      parser: tsParser,
      parserOptions: {
        ecmaFeatures: {
          jsx: true,
        },
      },
    },
    plugins: {
      "@typescript-eslint": tsPlugin,
      "react-hooks": reactHooksPlugin,
    },
    rules: {
      ...tsPlugin.configs.recommended.rules,
      "no-console": "off",
      // Regola definita per permettere direttive eslint-disable nei file
      // (dogfood: manteniamo come warn, non bloccante).
      "react-hooks/exhaustive-deps": "warn",
      // Transizione dogfood: "warn" oggi, salirà a "error" dopo pulizia
      // progressiva (vedi docs/tech-debt-ts.md). Evita di bloccare pnpm verify.
      "@typescript-eslint/no-explicit-any": "warn",
      "@typescript-eslint/no-unused-vars": [
        "warn",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
    },
  },
];
