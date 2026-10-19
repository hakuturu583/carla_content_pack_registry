// Cookie sessions, CSRF, OAuth state handling and deployment guards.
import { describe, expect, it } from 'vitest';
import { MemoryRepository } from '../../src/repository/memory.ts';
import { loadConfig } from '../../src/runtime/config.ts';
import { inProcess, testEnv } from '../helpers/harness.ts';

function cookieValue(setCookie: string | null, name: string): string {
  const m = new RegExp(`${name}=([^;]*)`).exec(setCookie ?? '');
  return m?.[1] ?? '';
}

describe('config guards', () => {
  it('forbids mock auth outside local', () => {
    expect(() => loadConfig(testEnv({ ENVIRONMENT: 'staging', PUBLIC_BASE_URL: 'https://x.example', LOCAL_S3_ENDPOINT: undefined }))).toThrow(
      /mock is forbidden/,
    );
    expect(() =>
      loadConfig(testEnv({ ENVIRONMENT: 'production', PUBLIC_BASE_URL: 'https://x.example', LOCAL_S3_ENDPOINT: undefined })),
    ).toThrow(/mock is forbidden/);
  });

  it('forbids local storage and plain http outside local', () => {
    const base = { ENVIRONMENT: 'production', AUTH_MODE: 'github', GITHUB_CLIENT_ID: 'id', GITHUB_CLIENT_SECRET: 's' };
    expect(() => loadConfig(testEnv({ ...base, PUBLIC_BASE_URL: 'https://x.example' }))).toThrow(/LOCAL_S3_ENDPOINT/);
    expect(() => loadConfig(testEnv({ ...base, PUBLIC_BASE_URL: 'http://x.example', LOCAL_S3_ENDPOINT: undefined }))).toThrow(/https/);
    expect(loadConfig(testEnv({ ...base, PUBLIC_BASE_URL: 'https://x.example', LOCAL_S3_ENDPOINT: undefined })).authMode).toBe('github');
  });

  it('caps presigned URL lifetime at 5 minutes', () => {
    expect(() => loadConfig(testEnv({ PRESIGN_TTL_SECONDS: '3600' }))).toThrow();
    expect(loadConfig(testEnv()).presignTtlSeconds).toBe(300);
  });

  it('a misconfigured deployment fails closed on every request', async () => {
    const { createApp } = await import('../../src/http/app.ts');
    const app = createApp(() => loadConfig({ ENVIRONMENT: 'production', AUTH_MODE: 'mock' }) as never);
    const r = await app.request('http://x/health');
    expect(r.status).toBe(500);
  });
});

describe('cookie session + CSRF (mock login)', () => {
  it('requires the CSRF token and same origin for state-changing requests', async () => {
    const { app } = inProcess(new MemoryRepository());
    const login = await app.request('http://localhost:8787/login/mock?user=alice');
    expect(login.status).toBe(302);
    const sc = login.headers.get('set-cookie');
    expect(sc).toMatch(/HttpOnly/);
    expect(sc).toMatch(/SameSite=Lax/);
    const cookie = `ccpr_session=${cookieValue(sc, 'ccpr_session')}`;

    const me = await app.request('http://localhost:8787/v1/me', { headers: { cookie } });
    const { csrfToken, user } = (await me.json()) as { csrfToken: string; user: { login: string } };
    expect(user.login).toBe('alice');
    expect(csrfToken).toBeTruthy();

    const body = JSON.stringify({ name: 'x', storage: { provider: 'local', bucket: 'alice-packs' }, credentials: { accessKeyId: 'LOCALALICEKEY00001', secretAccessKey: 'local-alice-secret-not-for-production' } });
    const post = (headers: Record<string, string>) =>
      app.request('http://localhost:8787/v1/buckets', { method: 'POST', body, headers: { cookie, 'content-type': 'application/json', ...headers } });
    expect((await post({})).status).toBe(403);
    expect((await post({ 'x-csrf-token': 'wrong' })).status).toBe(403);
    expect((await post({ 'x-csrf-token': csrfToken, origin: 'https://evil.example' })).status).toBe(403);
    expect((await post({ 'x-csrf-token': csrfToken, origin: 'http://localhost:8787' })).status).toBe(201);

    // logout requires CSRF too, then the session is gone
    expect((await app.request('http://localhost:8787/logout', { method: 'POST', headers: { cookie } })).status).toBe(403);
    const out = await app.request('http://localhost:8787/logout', { method: 'POST', headers: { cookie, 'x-csrf-token': csrfToken } });
    expect(out.status).toBe(204);
    expect((await app.request('http://localhost:8787/v1/me', { headers: { cookie } })).status).toBe(401);
  });

  it('keeps redirects under a PUBLIC_BASE_URL path prefix (API Gateway stage URLs)', async () => {
    const { app } = inProcess(new MemoryRepository(), testEnv({ PUBLIC_BASE_URL: 'http://localhost:8787/staging' }));
    const r = await app.request('http://localhost:8787/login/mock?user=alice');
    expect(r.headers.get('location')).toBe('/staging/');
    expect(await (await app.request('http://localhost:8787/')).text()).toContain('<base href="/staging/">');
  });

  it('stores the GitHub token encrypted, never in plaintext', async () => {
    const repo = new MemoryRepository();
    const { app } = inProcess(repo);
    await app.request('http://localhost:8787/login/mock?user=bob');
    const [s] = [...repo.sessions.values()];
    expect(s!.tokenCiphertext).not.toContain('mock:bob');
    expect(s!.tokenCiphertext.startsWith('v1.')).toBe(true);
  });
});

describe('GitHub OAuth web flow', () => {
  const githubEnv = testEnv({ AUTH_MODE: 'github', GITHUB_CLIENT_ID: 'cid', GITHUB_CLIENT_SECRET: 'csecret' });

  function githubFetch() {
    return (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input instanceof Request ? input.url : input);
      if (url === 'https://github.com/login/oauth/access_token') {
        const b = JSON.parse(String(init?.body));
        return Response.json(b.code === 'good' && b.code_verifier ? { access_token: 'gho_token' } : { error: 'bad_verification_code' });
      }
      if (url === 'https://api.github.com/user') return Response.json({ id: 4242, login: 'octo', name: null, avatar_url: null });
      return new Response('{}', { status: 404 });
    }) as typeof fetch;
  }

  it('redirects to GitHub with state and PKCE, then validates state on callback', async () => {
    const { app } = inProcess(new MemoryRepository(), githubEnv, githubFetch());
    const start = await app.request('http://localhost:8787/login/github?redirect_to=/buckets');
    expect(start.status).toBe(302);
    const loc = new URL(start.headers.get('location')!);
    const state = loc.searchParams.get('state')!;
    const stateCookie = `ccpr_oauth=${cookieValue(start.headers.get('set-cookie'), 'ccpr_oauth')}`;

    // wrong state => CSRF failure
    const bad = await app.request(`http://localhost:8787/login/github/callback?code=good&state=forged`, { headers: { cookie: stateCookie } });
    expect(bad.status).toBe(403);
    // missing cookie => failure
    expect((await app.request(`http://localhost:8787/login/github/callback?code=good&state=${state}`)).status).toBe(403);

    const ok = await app.request(`http://localhost:8787/login/github/callback?code=good&state=${state}`, { headers: { cookie: stateCookie } });
    expect(ok.status).toBe(302);
    expect(ok.headers.get('location')).toBe('/buckets');
    expect(ok.headers.get('set-cookie')).toMatch(/ccpr_session=/);
  });

  it('never redirects off-site after login', async () => {
    const { app } = inProcess(new MemoryRepository(), githubEnv, githubFetch());
    for (const target of ['https://evil.example', '//evil.example', '/\\evil.example']) {
      const start = await app.request(`http://localhost:8787/login/github?redirect_to=${encodeURIComponent(target)}`);
      const state = new URL(start.headers.get('location')!).searchParams.get('state');
      const cookie = `ccpr_oauth=${cookieValue(start.headers.get('set-cookie'), 'ccpr_oauth')}`;
      const cb = await app.request(`http://localhost:8787/login/github/callback?code=good&state=${state}`, { headers: { cookie } });
      expect(cb.headers.get('location')).toBe('/');
    }
  });

  it('does not expose the mock login in github mode', async () => {
    const { app } = inProcess(new MemoryRepository(), githubEnv, githubFetch());
    expect((await app.request('http://localhost:8787/login/mock?user=alice')).status).toBe(404);
  });
});

describe('membership failures deny org/team access', () => {
  it('returns 403 membership_unverifiable instead of granting', async () => {
    const repo = new MemoryRepository();
    const h = inProcess(repo);
    const reg = await h.client.call('POST', '/v1/buckets', {
      as: 'alice',
      body: { name: 'x', storage: { provider: 'local', bucket: 'alice-packs' }, credentials: { accessKeyId: 'LOCALALICEKEY00001', secretAccessKey: 'local-alice-secret-not-for-production' } },
    });
    await h.client.call('PUT', `/v1/buckets/${reg.body.id}/acl`, { as: 'alice', body: { entries: [{ type: 'org', login: 'carla-sim' }] } });
    // Make GitHub membership lookups fail for bob
    const identity = (h.services.core as unknown as { deps: { identity: { getMemberships: unknown } } }).deps.identity;
    identity.getMemberships = async () => {
      const { RegistryError } = await import('../../src/core/errors.ts');
      throw new RegistryError('membership_unverifiable', 'GitHub down');
    };
    const r = await h.client.call('GET', `/v1/buckets/${reg.body.id}`, { as: 'bob' });
    expect(r.status).toBe(403);
    expect(r.body.error.code).toBe('membership_unverifiable');
    expect(repo.audit.some((e) => e.outcome === 'deny' && e.detail?.reason === 'membership_unverifiable')).toBe(true);
  });
});
