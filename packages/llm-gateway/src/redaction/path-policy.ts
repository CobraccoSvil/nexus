import { minimatch } from "minimatch";

export interface PathPolicyConfig {
  whitelist: string[];  // pattern che escono senza redaction
  blacklist: string[];  // pattern mai inviabili a provider esterni
}

const DEFAULT_BLACKLIST = [
  "**/.env",
  "**/.env.*",
  "**/secrets/**",
  "**/customers/*/private/**",
  "**/*.pem",
  "**/*.key",
  "**/*.p12",
  "**/*.pfx",
  "**/*_rsa",
  "**/*_ed25519",
  "**/id_rsa",
  "**/id_ed25519",
  "**/credentials.json",
  "**/service-account*.json",
];

const DEFAULT_WHITELIST = [
  "**/README*",
  "**/docs/**/*.md",
  "**/LICENSE*",
  "**/CHANGELOG*",
  "**/node_modules/**",
  "**/*.lock",
];

export class PathPolicy {
  private config: PathPolicyConfig;

  constructor(config: Partial<PathPolicyConfig> = {}) {
    this.config = {
      whitelist: config.whitelist ?? DEFAULT_WHITELIST,
      blacklist: config.blacklist ?? DEFAULT_BLACKLIST,
    };
  }

  // Ritorna true se il file è in blacklist — blocca completamente l'invio
  isBlocked(filePath: string): boolean {
    return this.config.blacklist.some((pattern) =>
      minimatch(filePath, pattern, { dot: true })
    );
  }

  // Ritorna true se il file è in whitelist — passa senza redaction
  isWhitelisted(filePath: string): boolean {
    return this.config.whitelist.some((pattern) =>
      minimatch(filePath, pattern, { dot: true })
    );
  }

  checkPath(filePath: string): "blocked" | "whitelisted" | "redact" {
    if (this.isBlocked(filePath)) return "blocked";
    if (this.isWhitelisted(filePath)) return "whitelisted";
    return "redact";
  }
}
