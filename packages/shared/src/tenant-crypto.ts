import { createCipheriv, createDecipheriv, randomBytes } from "crypto";

export interface EncryptedBlob {
  ciphertext: string;  // base64 (AES-256-GCM ciphertext + 16-byte auth tag)
  iv: string;          // base64, 12 byte GCM nonce
  keyVersion: string;
}

export interface KMSBackend {
  getOrCreateKey(tenantId: string): Buffer | Promise<Buffer>;
  deleteKey(tenantId: string): void | Promise<void>;
  hasKey(tenantId: string): boolean | Promise<boolean>;
}

// Implementazione locale (dev/test). In prod: sostituire con AwsKMSBackend o VaultBackend.
export class LocalKeyStore implements KMSBackend {
  private keys = new Map<string, Buffer>();

  getOrCreateKey(tenantId: string): Buffer {
    if (!this.keys.has(tenantId)) {
      this.keys.set(tenantId, randomBytes(32));
    }
    return this.keys.get(tenantId)!;
  }

  deleteKey(tenantId: string): void {
    this.keys.delete(tenantId);
  }

  hasKey(tenantId: string): boolean {
    return this.keys.has(tenantId);
  }
}

export class TenantCrypto {
  constructor(private kms: KMSBackend = new LocalKeyStore()) {}

  async encrypt(tenantId: string, plaintext: string): Promise<EncryptedBlob> {
    const key = await this.kms.getOrCreateKey(tenantId);
    const iv = randomBytes(12);
    const cipher = createCipheriv("aes-256-gcm", key, iv);
    const encrypted = Buffer.concat([
      cipher.update(plaintext, "utf8"),
      cipher.final(),
    ]);
    const authTag = cipher.getAuthTag(); // 16 byte
    return {
      ciphertext: Buffer.concat([encrypted, authTag]).toString("base64"),
      iv: iv.toString("base64"),
      keyVersion: "v1",
    };
  }

  async decrypt(tenantId: string, blob: EncryptedBlob): Promise<string> {
    const key = await this.kms.getOrCreateKey(tenantId);
    const iv = Buffer.from(blob.iv, "base64");
    const combined = Buffer.from(blob.ciphertext, "base64");
    const ciphertext = combined.subarray(0, combined.length - 16);
    const authTag = combined.subarray(combined.length - 16);

    const decipher = createDecipheriv("aes-256-gcm", key, iv);
    decipher.setAuthTag(authTag);
    return decipher.update(ciphertext).toString("utf8") + decipher.final("utf8");
  }

  // Crypto-shredding: distrugge la chiave del tenant.
  // Tutti i blob cifrati con quella chiave diventano irrecuperabili
  // senza toccare i dati cifrati nel DB.
  async shredTenant(tenantId: string): Promise<void> {
    await this.kms.deleteKey(tenantId);
  }

  async hasTenant(tenantId: string): Promise<boolean> {
    return this.kms.hasKey(tenantId);
  }
}
