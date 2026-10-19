import { describe, expect, it } from 'vitest';
import { GitHubIdentityProvider, GitHubOAuth, nextLink } from '../../src/identity/github.ts';

type Handler = (url: URL, init?: RequestInit) => Response | Promise<Response>;
const json = (body: unknown, init: ResponseInit = {}) =>
  new Response(JSON.stringify(body), { status: 200, ...init, headers: { 'content-type': 'application/json', ...(init.headers ?? {}) } });

function fakeFetch(h: Handler) {
  const calls: string[] = [];
  const f = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(input instanceof Request ? input.url : String(input));
    calls.push(url.pathname + url.search);
    return h(url, init);
  }) as typeof fetch;
  return { f, calls };
}

describe('GitHubIdentityProvider.getMemberships', () => {
  it('follows Link pagination and uses numeric ids', async () => {
    const { f, calls } = fakeFetch((url) => {
      if (url.pathname === '/user/memberships/orgs') {
        if (url.searchParams.get('page') === '2') return json([{ state: 'active', organization: { id: 3 } }]);
        return json(
          [
            { state: 'active', organization: { id: 1 } },
            { state: 'pending', organization: { id: 2 } },
          ],
          { headers: { link: '<https://api.github.com/user/memberships/orgs?state=active&per_page=100&page=2>; rel="next"' } },
        );
      }
      if (url.pathname === '/user/teams') return json([{ id: 77 }]);
      return new Response('', { status: 404 });
    });
    const gh = new GitHubIdentityProvider('test', f);
    expect(await gh.getMemberships('t')).toEqual({ orgIds: [1, 3], teamIds: [77] });
    expect(calls.filter((c) => c.startsWith('/user/memberships/orgs'))).toHaveLength(2);
  });

  it('denies (membership_unverifiable) when GitHub refuses, e.g. org has not approved the OAuth App', async () => {
    const { f } = fakeFetch(() => json({ message: 'forbidden' }, { status: 403 }));
    await expect(new GitHubIdentityProvider('test', f).getMemberships('t')).rejects.toMatchObject({ code: 'membership_unverifiable' });
  });

  it('maps rate limiting to upstream_unavailable', async () => {
    const { f } = fakeFetch(() => json({}, { status: 403, headers: { 'x-ratelimit-remaining': '0' } }));
    await expect(new GitHubIdentityProvider('test', f).getMemberships('t')).rejects.toMatchObject({ code: 'upstream_unavailable' });
  });

  it('maps network failure to upstream_unavailable', async () => {
    const f = (async () => {
      throw new TypeError('network');
    }) as unknown as typeof fetch;
    await expect(new GitHubIdentityProvider('test', f).getMemberships('t')).rejects.toMatchObject({ code: 'upstream_unavailable' });
  });
});

describe('GitHubIdentityProvider.getProfile / resolvePrincipal', () => {
  it('rejects invalid tokens', async () => {
    const { f } = fakeFetch(() => json({}, { status: 401 }));
    await expect(new GitHubIdentityProvider('test', f).getProfile('bad')).rejects.toMatchObject({ code: 'unauthenticated' });
  });

  it('resolves teams to numeric ids and rejects orgs passed as users', async () => {
    const { f } = fakeFetch((url) => {
      if (url.pathname === '/orgs/carla/teams/maps') return json({ id: 42, slug: 'maps', organization: { login: 'carla' } });
      if (url.pathname === '/users/carla') return json({ id: 9, login: 'carla', type: 'Organization' });
      return json({}, { status: 404 });
    });
    const gh = new GitHubIdentityProvider('test', f);
    expect(await gh.resolvePrincipal('t', { type: 'team', org: 'carla', slug: 'maps' })).toEqual({ type: 'team', id: 42, name: 'carla/maps' });
    await expect(gh.resolvePrincipal('t', { type: 'user', login: 'carla' })).rejects.toMatchObject({ code: 'bad_request' });
    await expect(gh.resolvePrincipal('t', { type: 'org', login: 'nope' })).rejects.toMatchObject({ code: 'bad_request' });
  });
});

describe('nextLink', () => {
  it('only follows api.github.com links', () => {
    expect(nextLink('<https://evil.example/x>; rel="next"')).toBeNull();
    expect(nextLink('<https://api.github.com/user/teams?page=2>; rel="next", <https://api.github.com/user/teams?page=5>; rel="last"')).toBe(
      'https://api.github.com/user/teams?page=2',
    );
  });
});

describe('GitHubOAuth', () => {
  it('builds an authorize URL with state, PKCE and read:org', () => {
    const o = new GitHubOAuth({ clientId: 'cid', clientSecret: 's', redirectUri: 'https://r.example/login/github/callback' });
    const u = new URL(o.authorizeUrl('st', 'ch'));
    expect(u.origin + u.pathname).toBe('https://github.com/login/oauth/authorize');
    expect(Object.fromEntries(u.searchParams)).toMatchObject({
      client_id: 'cid',
      state: 'st',
      scope: 'read:org',
      code_challenge: 'ch',
      code_challenge_method: 'S256',
    });
  });

  it('exchanges the code with the verifier', async () => {
    let sent: Record<string, string> = {};
    const f = (async (_u: string, init: RequestInit) => {
      sent = JSON.parse(String(init.body));
      return json({ access_token: 'gho_x' });
    }) as unknown as typeof fetch;
    const o = new GitHubOAuth({ clientId: 'cid', clientSecret: 's', redirectUri: 'https://r/cb' }, f);
    expect(await o.exchangeCode('code', 'ver')).toBe('gho_x');
    expect(sent).toMatchObject({ code: 'code', code_verifier: 'ver', client_id: 'cid' });
  });

  it('fails on OAuth errors', async () => {
    const f = (async () => json({ error: 'bad_verification_code' })) as unknown as typeof fetch;
    await expect(new GitHubOAuth({ clientId: 'c', clientSecret: 's', redirectUri: 'https://r/cb' }, f).exchangeCode('x', 'y')).rejects.toMatchObject({
      code: 'unauthenticated',
    });
  });
});
