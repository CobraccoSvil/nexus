export class NexusError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly statusCode: number = 500,
    readonly details?: Record<string, unknown>
  ) {
    super(message);
    this.name = "NexusError";
  }

  toJSON() {
    return {
      code: this.code,
      message: this.message,
      statusCode: this.statusCode,
      details: this.details,
    };
  }
}

export class ConfigError extends NexusError {
  constructor(message: string, details?: Record<string, unknown>) {
    super("CONFIG_ERROR", message, 400, details);
    this.name = "ConfigError";
  }
}

export class ProviderError extends NexusError {
  constructor(
    message: string,
    readonly provider: string,
    readonly statusCode: number = 503,
    details?: Record<string, unknown>
  ) {
    super("PROVIDER_ERROR", message, statusCode, details);
    this.name = "ProviderError";
  }
}

export class RedactionError extends NexusError {
  constructor(message: string, details?: Record<string, unknown>) {
    super("REDACTION_ERROR", message, 500, details);
    this.name = "RedactionError";
  }
}

export class RateLimitError extends NexusError {
  constructor(
    message: string,
    readonly retryAfterMs: number,
    details?: Record<string, unknown>
  ) {
    super("RATE_LIMIT", message, 429, details);
    this.name = "RateLimitError";
  }
}

export class AuthError extends NexusError {
  constructor(message: string, details?: Record<string, unknown>) {
    super("AUTH_ERROR", message, 401, details);
    this.name = "AuthError";
  }
}

export class DLPBlockedError extends NexusError {
  constructor(
    message: string,
    readonly patternType: string,
    details?: Record<string, unknown>
  ) {
    super("DLP_BLOCKED", message, 403, details);
    this.name = "DLPBlockedError";
  }
}
