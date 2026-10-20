// GitHubIdentityProvider: OAuth App login, profile, and org/team membership checks.
// Anything GitHub cannot positively confirm is treated as "no access".
import { RegistryError, badRequest, unauthenticated } from '../core/errors.ts';
import type { IdentityProfile, IdentityProvider } from '../core/ports.ts';
import type { AclEntry, AclRef, Memberships } from '../core/types.ts';

const API = 'https://api.github.com';
const MAX_PAGES = 10; // 1000 orgs/teams; truncation can only cause denials, never grants

export interface GitHubOAuthConfig {
  clientId: string;
  clientSecret: string;
  redirectUri: string;
}

/** Scope needed to read the user's org and team memberships. */
export const OAUTH_SCOPES = 'read:org';

export class GitHubIdentityProvider implements IdentityProvider {
  constructor(
    private readonly userAgent: string,
    private readonly fetchImpl: typeof fetch = (...a) => fetch(...a),
  ) {}

  private async api(token: string, path: string): Promise<Response> {
    let res: Response;
    try {
      res = await this.fetchImpl(path.startsWith('http') ? path : `${API}${path}`, {
        headers: {
          authorization: `Bearer ${token}`,
          accept: 'application/vnd.github+json',
          'x-github-api-version': '2022-11-28',
          'user-agent': this.userAgent,
        },
      });
    } catch {
      throw new RegistryError('upstream_unavailable', 'GitHub API is unreachable');
    }
    if (res.status === 401) throw unauthenticated('GitHub token is invalid or expired');
    if ((res.status === 403 || res.status === 429) && (res.headers.get('x-ratelimit-remaining') === '0' || res.headers.has('retry-after'))) {
      throw new RegistryError('upstream_unavailable', 'GitHub API rate limit exceeded; try again later');
    }
    return res;
  }

  async getProfile(token: string): Promise<IdentityProfile> {
    const res = await this.api(token, '/user');
    if (!res.ok) throw new RegistryError('upstream_unavailable', `GitHub /user failed (HTTP ${res.status})`);
    const u = (await res.json()) as { id: number; login: string; name: string | null; avatar_url: string | null };
    if (typeof u.id !== 'number' || typeof u.login !== 'string') throw new RegistryError('upstream_unavailable', 'Unexpected GitHub response');
    return { id: u.id, login: u.login, name: u.name ?? null, avatarUrl: u.avatar_url ?? null };
  }

  private async paginate<T>(token: string, path: string): Promise<T[]> {
    const out: T[] = [];
    let url: string | null = `${API}${path}${path.includes('?') ? '&' : '?'}per_page=100`;
    for (let page = 0; url && page < MAX_PAGES; page++) {
      const res = await this.api(token, url);
      if (!res.ok) {
        throw new RegistryError(
          'membership_unverifiable',
          res.status === 403
            ? 'GitHub denied the membership query (missing read:org scope, or the organization has not approved this OAuth App)'
            : `GitHub membership query failed (HTTP ${res.status})`,
        );
      }
      const body = (await res.json()) as T[];
      if (!Array.isArray(body)) throw new RegistryError('membership_unverifiable', 'Unexpected GitHub response');
      out.push(...body);
      url = nextLink(res.headers.get('link'));
    }
    return out;
  }

  async getMemberships(token: string): Promise<Memberships> {
    const [orgs, teams] = await Promise.all([
      this.paginate<{ state: string; organization: { id: number } }>(token, '/user/memberships/orgs?state=active'),
      this.paginate<{ id: number }>(token, '/user/teams'),
    ]);
    return {
      orgIds: orgs.filter((m) => m.state === 'active').map((m) => m.organization.id),
      teamIds: teams.map((t) => t.id),
    };
  }

  async resolvePrincipal(token: string, ref: AclRef): Promise<AclEntry> {
    const path =
      ref.type === 'user'
        ? `/users/${encodeURIComponent(ref.login)}`
        : ref.type === 'org'
          ? `/orgs/${encodeURIComponent(ref.login)}`
          : `/orgs/${encodeURIComponent(ref.org)}/teams/${encodeURIComponent(ref.slug)}`;
    const res = await this.api(token, path);
    const label = ref.type === 'team' ? `${ref.org}/${ref.slug}` : ref.login;
    if (res.status === 404 || res.status === 403) {
      throw badRequest(
        `GitHub ${ref.type} "${label}" was not found or is not visible to you` +
          (ref.type === 'team' ? ' (the organization may need to approve this OAuth App)' : ''),
      );
    }
    if (!res.ok) throw new RegistryError('upstream_unavailable', `GitHub lookup failed (HTTP ${res.status})`);
    const body = (await res.json()) as { id: number; login?: string; slug?: string; type?: string; organization?: { login: string } };
    if (ref.type === 'user' && body.type !== 'User') throw badRequest(`"${label}" is not a GitHub user`);
    const name = ref.type === 'team' ? `${body.organization?.login ?? ref.org}/${body.slug ?? ref.slug}` : (body.login ?? label);
    return { type: ref.type, id: body.id, name };
  }
}

export function nextLink(link: string | null): string | null {
  if (!link) return null;
  for (const part of link.split(',')) {
    const m = /<([^>]+)>;\s*rel="next"/.exec(part);
    if (m && m[1]!.startsWith(`${API}/`)) return m[1]!;
  }
  return null;
}

/** OAuth App web flow (authorization code + PKCE). */
export class GitHubOAuth {
  constructor(
    private readonly cfg: GitHubOAuthConfig,
    private readonly fetchImpl: typeof fetch = (...a) => fetch(...a),
  ) {}

  authorizeUrl(state: string, codeChallenge: string): string {
    const u = new URL('https://github.com/login/oauth/authorize');
    u.searchParams.set('client_id', this.cfg.clientId);
    u.searchParams.set('redirect_uri', this.cfg.redirectUri);
    u.searchParams.set('scope', OAUTH_SCOPES);
    u.searchParams.set('state', state);
    u.searchParams.set('code_challenge', codeChallenge);
    u.searchParams.set('code_challenge_method', 'S256');
    u.searchParams.set('allow_signup', 'false');
    return u.toString();
  }

  async exchangeCode(code: string, codeVerifier: string): Promise<string> {
    let res: Response;
    try {
      res = await this.fetchImpl('https://github.com/login/oauth/access_token', {
        method: 'POST',
        headers: { accept: 'application/json', 'content-type': 'application/json' },
        body: JSON.stringify({
          client_id: this.cfg.clientId,
          client_secret: this.cfg.clientSecret,
          code,
          redirect_uri: this.cfg.redirectUri,
          code_verifier: codeVerifier,
        }),
      });
    } catch {
      throw new RegistryError('upstream_unavailable', 'GitHub OAuth is unreachable');
    }
    const body = (await res.json().catch(() => ({}))) as { access_token?: string; error?: string };
    if (!res.ok || !body.access_token) throw unauthenticated(`GitHub OAuth failed${body.error ? `: ${body.error}` : ''}`);
    return body.access_token;
  }
}
