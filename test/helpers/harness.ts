import { systemClock, type RegistryRepository } from '../../src/core/ports.ts';
import { createApp } from '../../src/http/app.ts';
import { loadConfig, type RawEnv } from '../../src/runtime/config.ts';
import { buildServices, type Services } from '../../src/runtime/services.ts';
import { MemoryRateLimiter } from '../../src/util/ratelimit.ts';
import { FAKE_S3_ENDPOINT, createFakeS3 } from './fake-s3.ts';
import type { ApiClient } from './scenarios.ts';

// 32 zero-ish bytes; test-only key.
export const TEST_KEYS = `test:${Buffer.alloc(32, 7).toString('base64')}`;

export function testEnv(extra: RawEnv = {}): RawEnv {
  return {
    ENVIRONMENT: 'local',
    AUTH_MODE: 'mock',
    PUBLIC_BASE_URL: 'http://localhost:8787',
    CREDENTIAL_KEYS: TEST_KEYS,
    LOCAL_S3_ENDPOINT: FAKE_S3_ENDPOINT,
    ...extra,
  };
}

export function inProcess(repo: RegistryRepository, env: RawEnv = testEnv(), fetchImpl?: typeof fetch) {
  const s3 = createFakeS3();
  const f = fetchImpl ?? s3.fetch;
  const services: Services = buildServices(loadConfig(env), {
    runtime: 'test',
    repo,
    rateLimiter: new MemoryRateLimiter(systemClock, { write: { limit: 1000, windowSeconds: 60 }, read: { limit: 1000, windowSeconds: 60 } }),
    awsIdentity: () => null,
    fetchImpl: f,
  });
  const app = createApp(() => services);
  const client: ApiClient = {
    async call(method, path, opts = {}) {
      const headers: Record<string, string> = { ...(opts.headers ?? {}) };
      if (opts.as) headers.authorization = `Bearer mock:${opts.as}`;
      let body: string | undefined;
      if (opts.body !== undefined) {
        headers['content-type'] = 'application/json';
        body = JSON.stringify(opts.body);
      }
      const res = await app.request(`http://localhost:8787${path}`, { method, headers, body });
      const text = await res.text();
      let parsed: unknown = null;
      try {
        parsed = text ? JSON.parse(text) : null;
      } catch {
        parsed = text;
      }
      return { status: res.status, body: parsed, headers: res.headers };
    },
    async fetchUrl(url) {
      const r = await f(url);
      return { status: r.status, text: await r.text() };
    },
  };
  return { app, services, client, s3 };
}
