import { resolve } from "path";
import { fileURLToPath } from "url";

const __dirname = fileURLToPath(new URL(".", import.meta.url));

// Sposta CWD alla root del monorepo prima dei test
// in modo che i path relativi come ./config/policies/... funzionino
export default function () {
  process.setMaxListeners(20);
  process.chdir(resolve(__dirname, "../../.."));
}
