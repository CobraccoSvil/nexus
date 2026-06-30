import { SignJWT, jwtVerify, type JWTPayload } from "jose";
import { AuthError } from "./errors.js";

export interface NexusTokenPayload extends JWTPayload {
  tid: string;    // tenant_id
  uid: string;    // user_id
  scp: string[];  // scopes: ["llm:complete", "llm:stream", "rag:read", ...]
}

export class JWTService {
  private secret: Uint8Array;

  constructor(secretKey: string) {
    if (!secretKey || secretKey.length < 32) {
      throw new AuthError("JWT_SECRET deve essere almeno 32 caratteri");
    }
    this.secret = new TextEncoder().encode(secretKey);
  }

  async sign(
    payload: Omit<NexusTokenPayload, "iat" | "exp">,
    expiresIn: string = "1h"
  ): Promise<string> {
    return new SignJWT(payload as JWTPayload)
      .setProtectedHeader({ alg: "HS256" })
      .setIssuedAt()
      .setExpirationTime(expiresIn)
      .sign(this.secret);
  }

  async verify(token: string): Promise<NexusTokenPayload> {
    let payload: JWTPayload;
    try {
      ({ payload } = await jwtVerify(token, this.secret));
    } catch (err) {
      const msg = err instanceof Error ? err.message : "Token non valido";
      throw new AuthError(`JWT non valido: ${msg}`);
    }

    if (
      typeof payload["tid"] !== "string" ||
      typeof payload["uid"] !== "string" ||
      !Array.isArray(payload["scp"])
    ) {
      throw new AuthError("Token mancante di claim obbligatori: tid, uid, scp");
    }

    return payload as NexusTokenPayload;
  }

  hasScope(payload: NexusTokenPayload, required: string): boolean {
    return payload.scp.includes(required);
  }

  requireScope(payload: NexusTokenPayload, required: string): void {
    if (!this.hasScope(payload, required)) {
      throw new AuthError(`Scope mancante: ${required}`, { required, present: payload.scp });
    }
  }
}
