import { describe, expect, it } from 'vitest';
import { Keyring } from '../../src/util/crypto.ts';

const k = (b: number) => Buffer.alloc(32, b).toString('base64');

describe('Keyring', () => {
  it('round-trips and binds ciphertext to purpose and AAD', async () => {
    const kr = Keyring.parse(`k1:${k(1)}`);
    const ct = await kr.encrypt('storage-credentials', 'bucket:a', 'secret');
    expect(ct.startsWith('v1.k1.')).toBe(true);
    expect(ct).not.toContain('secret');
    expect(await kr.decrypt('storage-credentials', 'bucket:a', ct)).toBe('secret');
    await expect(kr.decrypt('storage-credentials', 'bucket:b', ct)).rejects.toThrow();
    await expect(kr.decrypt('oauth-token', 'bucket:a', ct)).rejects.toThrow();
  });

  it('detects tampering', async () => {
    const kr = Keyring.parse(`k1:${k(1)}`);
    const ct = await kr.encrypt('oauth-token', 'x', 'token');
    const parts = ct.split('.');
    const body = parts[3]!;
    parts[3] = (body[0] === 'A' ? 'B' : 'A') + body.slice(1);
    await expect(kr.decrypt('oauth-token', 'x', parts.join('.'))).rejects.toThrow();
  });

  it('supports rotation: new key encrypts, old key still decrypts', async () => {
    const old = Keyring.parse(`k1:${k(1)}`);
    const ct = await old.encrypt('oauth-token', 'x', 'token');
    const rotated = Keyring.parse(`k2:${k(2)},k1:${k(1)}`);
    expect(await rotated.decrypt('oauth-token', 'x', ct)).toBe('token');
    expect(rotated.needsRotation(ct)).toBe(true);
    expect((await rotated.encrypt('oauth-token', 'x', 't')).startsWith('v1.k2.')).toBe(true);
  });

  it('rejects malformed key specs', () => {
    expect(() => Keyring.parse(undefined)).toThrow();
    expect(() => Keyring.parse('k1:' + Buffer.alloc(16).toString('base64'))).toThrow(/32 bytes/);
    expect(() => Keyring.parse('nokey')).toThrow();
  });
});
