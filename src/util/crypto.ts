// AES-256-GCM envelope encryption with WebCrypto (available in Workers and Node 22).
// Ciphertext format: v1.<keyId>.<iv b64url>.<ciphertext b64url>
// Purpose-specific keys are derived from the master key with HKDF, and the
// associated data binds a ciphertext to its record (e.g. bucket id) so a stored
// blob cannot be swapped onto a different bucket.

const enc = new TextEncoder();
const dec = new TextDecoder();

export function b64urlEncode(bytes: Uint8Array): string {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

export function b64urlDecode(s: string): Uint8Array<ArrayBuffer> {
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/') + '==='.slice((s.length + 3) % 4);
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** Accepts standard base64 as well (the keyring config uses it). */
const b64Decode = (s: string) => b64urlDecode(s.replace(/=+$/, ''));

export function randomToken(bytes = 32): string {
  return b64urlEncode(crypto.getRandomValues(new Uint8Array(bytes)));
}

export async function sha256(input: string): Promise<Uint8Array> {
  return new Uint8Array(await crypto.subtle.digest('SHA-256', enc.encode(input)));
}

export async function sha256Hex(input: string): Promise<string> {
  return [...(await sha256(input))].map((b) => b.toString(16).padStart(2, '0')).join('');
}

export function timingSafeEqual(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let r = 0;
  for (let i = 0; i < a.length; i++) r |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return r === 0;
}

export type Purpose = 'storage-credentials' | 'oauth-token';

/**
 * Keyring parsed from `CREDENTIAL_KEYS`, e.g. "k2:<base64 32 bytes>,k1:<base64 32 bytes>".
 * The first key encrypts; all keys can decrypt (rotation).
 */
export class Keyring {
  private readonly derived = new Map<string, Promise<CryptoKey>>();
  private constructor(
    private readonly keys: Map<string, Uint8Array<ArrayBuffer>>,
    private readonly currentId: string,
  ) {}

  static parse(spec: string | undefined): Keyring {
    if (!spec) throw new Error('CREDENTIAL_KEYS is not configured');
    const keys = new Map<string, Uint8Array<ArrayBuffer>>();
    let first: string | null = null;
    for (const part of spec.split(',').map((s) => s.trim()).filter(Boolean)) {
      const idx = part.indexOf(':');
      if (idx <= 0) throw new Error('CREDENTIAL_KEYS entries must be "<id>:<base64>"');
      const id = part.slice(0, idx);
      if (!/^[A-Za-z0-9_-]{1,32}$/.test(id)) throw new Error('invalid key id in CREDENTIAL_KEYS');
      const raw = b64Decode(part.slice(idx + 1));
      if (raw.length !== 32) throw new Error(`key ${id} must be 32 bytes`);
      keys.set(id, raw);
      first ??= id;
    }
    if (!first) throw new Error('CREDENTIAL_KEYS is empty');
    return new Keyring(keys, first);
  }

  private key(id: string, purpose: Purpose): Promise<CryptoKey> {
    const cacheKey = `${id}/${purpose}`;
    let k = this.derived.get(cacheKey);
    if (!k) {
      const raw = this.keys.get(id);
      if (!raw) throw new Error('unknown key id');
      k = (async () => {
        const master = await crypto.subtle.importKey('raw', raw, 'HKDF', false, ['deriveKey']);
        return crypto.subtle.deriveKey(
          { name: 'HKDF', hash: 'SHA-256', salt: enc.encode('ccpr/v1'), info: enc.encode(purpose) },
          master,
          { name: 'AES-GCM', length: 256 },
          false,
          ['encrypt', 'decrypt'],
        );
      })();
      this.derived.set(cacheKey, k);
    }
    return k;
  }

  async encrypt(purpose: Purpose, aad: string, plaintext: string): Promise<string> {
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const key = await this.key(this.currentId, purpose);
    const ct = new Uint8Array(
      await crypto.subtle.encrypt({ name: 'AES-GCM', iv, additionalData: enc.encode(aad) }, key, enc.encode(plaintext)),
    );
    return `v1.${this.currentId}.${b64urlEncode(iv)}.${b64urlEncode(ct)}`;
  }

  async decrypt(purpose: Purpose, aad: string, envelope: string): Promise<string> {
    const parts = envelope.split('.');
    if (parts.length !== 4 || parts[0] !== 'v1') throw new Error('malformed ciphertext');
    const [, id, iv, ct] = parts as [string, string, string, string];
    const key = await this.key(id, purpose);
    const pt = await crypto.subtle.decrypt(
      { name: 'AES-GCM', iv: b64urlDecode(iv), additionalData: enc.encode(aad) },
      key,
      b64urlDecode(ct),
    );
    return dec.decode(pt);
  }

  /** True when the envelope was produced with an older key and should be re-sealed. */
  needsRotation(envelope: string): boolean {
    return envelope.split('.')[1] !== this.currentId;
  }
}
