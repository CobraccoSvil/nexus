import { defineConfig } from "vitest/config";
import { resolve } from "path";

export default defineConfig({
  test: {
    globals: true,
    environment: "node",
    include: ["tests/**/*.test.ts"],
    alias: {
      "@nexus/shared": resolve(__dirname, "../../shared/src/index.ts"),
    },
  },
});
