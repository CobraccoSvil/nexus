#!/usr/bin/env node
// scripts/check-no-honeypot-tsc.mjs — fail if npm honeypot package "tsc" is installed.

import { readFileSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const honeypotPkg = join(ROOT, "node_modules", "tsc", "package.json");

if (!existsSync(honeypotPkg)) {
  process.exit(0);
}

let name;
try {
  name = JSON.parse(readFileSync(honeypotPkg, "utf8")).name;
} catch {
  process.exit(0);
}

if (name === "tsc") {
  console.error(
    "verify: npm package 'tsc' is installed (honeypot, not the TypeScript compiler).\n" +
      "  Fix: pnpm remove tsc\n" +
      "  Use devDependency 'typescript' and run typecheck via pnpm (pnpm run typecheck).\n" +
      "  Never: npm install tsc or bare npx tsc without typescript in the project.",
  );
  process.exit(1);
}
