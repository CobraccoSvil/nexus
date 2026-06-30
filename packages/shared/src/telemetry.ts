import pino from "pino";
import type { Config } from "./config.js";

// Phase 0: Stub telemetry. Full OpenTelemetry integration in Phase 5.
export function initTelemetry(config: Config) {
  if (config.telemetry.enabled) {
    console.log(
      `[Telemetry] Initialized (stub). Full OTLP integration in Phase 5.`
    );
  }
}

export function shutdownTelemetry() {
  // Phase 5: Add proper shutdown
}

export function createLogger(config: Config) {
  const isDev = process.env.NODE_ENV !== "production";

  return pino(
    {
      level: config.telemetry.log_level,
      transport: isDev
        ? {
            target: "pino-pretty",
            options: {
              colorize: true,
              translateTime: "SYS:standard",
              ignore: "pid,hostname",
            },
          }
        : undefined,
    },
    pino.destination()
  );
}
