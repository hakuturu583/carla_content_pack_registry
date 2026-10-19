// Authentication: GitHub OAuth web flow (state + PKCE), server-side sessions with
// encrypted GitHub tokens, Bearer tokens for the CLI, and CSRF protection.
import { RegistryError, unauthenticated } from '../core/errors.ts';
import type { Clock, IdentityProvider, RegistryRepository } from '../core/ports.ts';
import type { Viewer } from '../core/registry.ts';
import type { Session } from '../core/types.ts';
import type { GitHubOAuth } from '../identity/github.ts';
import { type Keyring, b64urlEncode, randomToken, sha256Hex, timingSafeEqual } from '../util/crypto.ts';

export interface AuthDeps {
  repo: RegistryRepository;
  identity: IdentityProvider;
  oauth: GitHubOAuth | null;
  keyring: Keyring;
  clock: Clock;
  sessionTtlSeconds: number;
  secureCookies: boolean;
  publicOrigin: string;
}

export interface CookieSpec {
  name: string;
  value: string;
  maxAge: number;
  path?: string;
}

export interface AuthResult {
  viewer: Viewer;
  /** Set for cookie sessions; Bearer requests are not subject to CSRF. */
  session: Session | null;
}

const BEARER_CACHE_MS = 60_000;

export class AuthService {
  readonly sessionCookie: string;
  readonly stateCookie: string;
  private readonly bearerCache = new Map<string, { viewer: Viewer; until: number }>();

  constructor(private readonly d: AuthDeps) {
    // __Host- prefix pins the cookie to this exact origin (requires Secure).
    this.sessionCookie = d.secureCookies ? '__Host-ccpr_session' : 'ccpr_session';
    this.stateCookie = d.secureCookies ? '__Host-ccpr_oauth' : 'ccpr_oauth';
  }

  cookieHeader(c: CookieSpec): string {
    return [
      `${c.name}=${c.value}`,
      `Path=${c.path ?? '/'}`,
      `Max-Age=${c.maxAge}`,
      'HttpOnly',
      'SameSite=Lax',
      ...(this.d.secureCookies ? ['Secure'] : []),
    ].join('; ');
  }

  // ---- OAuth -----------------------------------------------------------------

  async startLogin(redirectTo: string | undefined): Promise<{ location: string; cookie: string }> {
    if (!this.d.oauth) throw new RegistryError('not_found', 'GitHub login is not enabled');
    const state = randomToken(24);
    const verifier = randomToken(32);
    const challenge = b64urlEncode(new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(verifier))));
    const payload = b64urlEncode(new TextEncoder().encode(JSON.stringify({ state, verifier, redirect: safeRedirect(redirectTo) })));
    return {
      location: this.d.oauth.authorizeUrl(state, challenge),
      cookie: this.cookieHeader({ name: this.stateCookie, value: payload, maxAge: 600 }),
    };
  }

  async finishLogin(
    query: { code?: string; state?: string; error?: string },
    stateCookie: string | undefined,
  ): Promise<{ cookies: string[]; redirect: string }> {
    if (!this.d.oauth) throw new RegistryError('not_found', 'GitHub login is not enabled');
    if (query.error) throw unauthenticated(`GitHub authorization was not granted (${query.error})`);
    if (!query.code || !query.state || !stateCookie) throw new RegistryError('csrf_failed', 'Missing OAuth state');
    let saved: { state: string; verifier: string; redirect: string };
    try {
      saved = JSON.parse(new TextDecoder().decode(b64urlDecodeSafe(stateCookie)));
    } catch {
      throw new RegistryError('csrf_failed', 'Invalid OAuth state');
    }
    if (typeof saved.state !== 'string' || !timingSafeEqual(saved.state, query.state)) {
      throw new RegistryError('csrf_failed', 'OAuth state mismatch');
    }
    const token = await this.d.oauth.exchangeCode(query.code, saved.verifier);
    const cookie = await this.createSession(token);
    return {
      cookies: [cookie, this.cookieHeader({ name: this.stateCookie, value: '', maxAge: 0 })],
      redirect: safeRedirect(saved.redirect),
    };
  }

  /** Creates a session for an already-obtained token (GitHub OAuth or mock). */
  async createSession(token: string): Promise<string> {
    const profile = await this.d.identity.getProfile(token);
    await this.d.repo.upsertUser(profile);
    const raw = randomToken(32);
    const idHash = await sha256Hex(raw);
    const now = this.d.clock.now();
    await this.d.repo.putSession({
      idHash,
      userId: profile.id,
      tokenCiphertext: await this.d.keyring.encrypt('oauth-token', `session:${idHash}`, token),
      csrfToken: randomToken(24),
      expiresAt: now + this.d.sessionTtlSeconds * 1000,
      createdAt: now,
    });
    return this.cookieHeader({ name: this.sessionCookie, value: raw, maxAge: this.d.sessionTtlSeconds });
  }

  async logout(sessionCookie: string | undefined): Promise<string> {
    if (sessionCookie) await this.d.repo.deleteSession(await sha256Hex(sessionCookie));
    return this.cookieHeader({ name: this.sessionCookie, value: '', maxAge: 0 });
  }

  // ---- Request authentication ------------------------------------------------

  async authenticate(headers: { authorization?: string; sessionCookie?: string }): Promise<AuthResult | null> {
    const auth = headers.authorization;
    if (auth) {
      const m = /^Bearer\s+(\S+)$/i.exec(auth);
      if (!m) throw unauthenticated('Malformed Authorization header');
      return { viewer: await this.bearer(m[1]!), session: null };
    }
    if (!headers.sessionCookie) return null;
    const idHash = await sha256Hex(headers.sessionCookie);
    const s = await this.d.repo.getSession(idHash);
    if (!s || s.expiresAt <= this.d.clock.now()) return null;
    const user = await this.d.repo.getUser(s.userId);
    if (!user) return null;
    let token: string;
    try {
      token = await this.d.keyring.decrypt('oauth-token', `session:${idHash}`, s.tokenCiphertext);
    } catch {
      return null;
    }
    return { viewer: { user, token }, session: s };
  }

  private async bearer(token: string): Promise<Viewer> {
    const key = await sha256Hex(token);
    const now = this.d.clock.now();
    const hit = this.bearerCache.get(key);
    if (hit && hit.until > now) return hit.viewer;
    const profile = await this.d.identity.getProfile(token);
    const user = await this.d.repo.upsertUser(profile);
    const viewer = { user, token };
    if (this.bearerCache.size > 1000) this.bearerCache.clear();
    this.bearerCache.set(key, { viewer, until: now + BEARER_CACHE_MS });
    return viewer;
  }

  /** CSRF for cookie sessions: same-origin Origin header + synchronizer token. */
  checkCsrf(session: Session, origin: string | undefined, token: string | undefined): void {
    if (origin && origin !== this.d.publicOrigin) throw new RegistryError('csrf_failed', 'Cross-origin request rejected');
    if (!token || !timingSafeEqual(token, session.csrfToken)) throw new RegistryError('csrf_failed', 'Missing or invalid X-CSRF-Token');
  }
}

/** Only same-site relative paths are allowed (open-redirect protection). */
export function safeRedirect(r: string | undefined): string {
  if (!r || !r.startsWith('/') || r.startsWith('//') || r.startsWith('/\\') || r.length > 512) return '/';
  return r;
}

function b64urlDecodeSafe(s: string): Uint8Array {
  if (!/^[A-Za-z0-9_-]{1,4096}$/.test(s)) throw new Error('bad cookie');
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/') + '==='.slice((s.length + 3) % 4);
  return Uint8Array.from(atob(b64), (ch) => ch.charCodeAt(0));
}
