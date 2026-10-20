import { describe, expect, it } from 'vitest';
import { MemoryRepository } from '../../src/repository/memory.ts';
import { MemoryRateLimiter } from '../../src/util/ratelimit.ts';
import { LOCAL_KEYS } from '../helpers/fake-s3.ts';
import { inProcess } from '../helpers/harness.ts';

describe('MemoryRateLimiter', () => {
  it('limits per key within a fixed window', async () => {
    let now = 0;
    const rl = new MemoryRateLimiter({ now: () => now }, { write: { limit: 2, windowSeconds: 60 }, read: { limit: 3, windowSeconds: 60 } });
    expect([await rl.limit('write', 'user:1'), await rl.limit('write', 'user:1'), await rl.limit('write', 'user:1')]).toEqual([true, true, false]);
    expect(await rl.limit('read', 'user:1')).toBe(true); // classes are counted separately
    expect(await rl.limit('write', 'user:2')).toBe(true); // other users unaffected
    now = 61_000;
    expect(await rl.limit('write', 'user:1')).toBe(true); // next window
  });
});

describe('API rate limiting', () => {
  it('returns 429 per GitHub user', async () => {
    const rateLimiter = new MemoryRateLimiter({ now: () => 0 }, { write: { limit: 1, windowSeconds: 60 }, read: { limit: 1, windowSeconds: 60 } });
    const { client } = inProcess(new MemoryRepository(), { rateLimiter });
    const body = { name: 'x', storage: { provider: 'local', bucket: 'alice-packs' }, credentials: LOCAL_KEYS.alice };
    expect((await client.call('POST', '/v1/buckets', { as: 'alice', body })).status).toBe(201);
    const limited = await client.call('POST', '/v1/buckets', { as: 'alice', body });
    expect(limited.status).toBe(429);
    expect(limited.body.error.code).toBe('rate_limited');
    expect((await client.call('POST', '/v1/buckets', { as: 'bob', body })).status).toBe(201);
  });
});
