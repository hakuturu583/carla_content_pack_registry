import { systemClock, type IdentityProvider, type RateLimiter, type RegistryRepository } from '../../src/core/ports.ts';
import { createApp } from '../../src/http/app.ts';
import { loadConfig, type RawEnv } from '../../src/runtime/config.ts';
import { buildServices, type Services } from '../../src/runtime/services.ts';
import { MemoryRateLimiter } from '../../src/util/ratelimit.ts';
import { FAKE_S3_ENDPOINT, createFakeS3 } from './fake-s3.ts';
import type { ApiClient, ApiResponse } from './scenarios.ts';

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

export interface InProcessOptions {
  env?: RawEnv;
  fetchImpl?: typeof fetch;
  rateLimiter?: RateLimiter;
  identity?: IdentityProvider;
}

/** Request headers for a test call: mock bearer auth for `as`, JSON content type for bodies. */
export function headersFor(opts: { as?: string; body?: unknown; headers?: Record<string, string> }): Record<string, string> {
  const headers: Record<string, string> = { ...(opts.headers ?? {}) };
  if (opts.as) headers.authorization = `Bearer mock:${opts.as}`;
  if (opts.body !== undefined) headers['content-type'] = 'application/json';
  return headers;
}

/** Response as the scenarios see it: JSON when parseable, otherwise the raw text. */
export async function parseResponse(res: Response): Promise<ApiResponse> {
  const text = await res.text();
  let body: unknown = text || null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    /* not JSON */
  }
  return { status: res.status, body, headers: res.headers };
}

export function inProcess(repo: RegistryRepository, opts: InProcessOptions = {}) {
  const s3 = createFakeS3();
  const f = opts.fetchImpl ?? s3.fetch;
  const services: Services = buildServices(loadConfig(opts.env ?? testEnv()), {
    runtime: 'test',
    repo,
    rateLimiter:
      opts.rateLimiter ??
      new MemoryRateLimiter(systemClock, { write: { limit: 1000, windowSeconds: 60 }, read: { limit: 1000, windowSeconds: 60 } }),
    awsIdentity: () => null,
    fetchImpl: f,
    identity: opts.identity,
  });
  const app = createApp(() => services);
  const client: ApiClient = {
    async call(method, path, callOpts = {}) {
      const body = callOpts.body === undefined ? undefined : JSON.stringify(callOpts.body);
      return parseResponse(await app.request(`http://localhost:8787${path}`, { method, headers: headersFor(callOpts), body }));
    },
    async fetchUrl(url) {
      const r = await f(url);
      return { status: r.status, text: await r.text() };
    },
  };
  return { app, services, client, s3 };
}
