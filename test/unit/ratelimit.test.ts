import { describe, expect, it } from 'vitest';
import { createApp } from '../../src/http/app.ts';
import { MemoryRepository } from '../../src/repository/memory.ts';
import { loadConfig } from '../../src/runtime/config.ts';
import { buildServices } from '../../src/runtime/services.ts';
import { MemoryRateLimiter, limitClass } from '../../src/util/ratelimit.ts';
import { createFakeS3 } from '../helpers/fake-s3.ts';
import { testEnv } from '../helpers/harness.ts';

describe('MemoryRateLimiter', () => {
  it('limits per key within a fixed window', async () => {
    let now = 0;
    const rl = new MemoryRateLimiter({ now: () => now }, { write: { limit: 2, windowSeconds: 60 }, read: { limit: 3, windowSeconds: 60 } });
    expect(limitClass('user:1:write')).toBe('write');
    expect(limitClass('user:1:bucket:x')).toBe('read');
    expect([await rl.limit('user:1:write'), await rl.limit('user:1:write'), await rl.limit('user:1:write')]).toEqual([true, true, false]);
    expect(await rl.limit('user:2:write')).toBe(true); // other users unaffected
    now = 61_000;
    expect(await rl.limit('user:1:write')).toBe(true); // next window
  });
});

describe('API rate limiting', () => {
  it('returns 429 per GitHub user and bucket', async () => {
    const s = buildServices(loadConfig(testEnv()), {
      runtime: 'test',
      repo: new MemoryRepository(),
      rateLimiter: new MemoryRateLimiter({ now: () => 0 }, { write: { limit: 1, windowSeconds: 60 }, read: { limit: 1, windowSeconds: 60 } }),
      awsIdentity: () => null,
      fetchImpl: createFakeS3().fetch,
    });
    const app = createApp(() => s);
    const body = JSON.stringify({
      name: 'x',
      storage: { provider: 'local', bucket: 'alice-packs' },
      credentials: { accessKeyId: 'LOCALALICEKEY00001', secretAccessKey: 'local-alice-secret-not-for-production' },
    });
    const post = (as: string) =>
      app.request('http://localhost:8787/v1/buckets', {
        method: 'POST',
        body,
        headers: { authorization: `Bearer mock:${as}`, 'content-type': 'application/json' },
      });
    expect((await post('alice')).status).toBe(201);
    const limited = await post('alice');
    expect(limited.status).toBe(429);
    expect(((await limited.json()) as { error: { code: string } }).error.code).toBe('rate_limited');
    expect((await post('bob')).status).toBe(201);
  });
});
