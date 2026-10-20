// HTTP API (Hono). Identical on Cloudflare Workers and AWS Lambda — the runtime
// only supplies Services. Contract: openapi/openapi.yaml
import { Hono, type Context } from 'hono';
import { bodyLimit } from 'hono/body-limit';
import { getCookie } from 'hono/cookie';
import { RegistryError, badRequest, notFound, unauthenticated } from '../core/errors.ts';
import type { Viewer } from '../core/registry.ts';
import type { Session } from '../core/types.ts';
import { MOCK_USERS } from '../identity/mock.ts';
import type { Services } from '../runtime/services.ts';
import { log } from '../util/log.ts';
import { APP_JS, indexHtml } from './ui.ts';

type Env = {
  Variables: {
    services: Services;
    viewer: Viewer;
    session: Session | null;
    requestId: string;
  };
};

export type ServicesProvider = (c: Context) => Services | Promise<Services>;

const UNSAFE = new Set(['POST', 'PUT', 'PATCH', 'DELETE']);
const BUCKET_ID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

async function jsonBody(c: Context): Promise<unknown> {
  const ct = c.req.header('content-type') ?? '';
  if (!ct.toLowerCase().startsWith('application/json')) throw badRequest('Content-Type must be application/json');
  try {
    return await c.req.json();
  } catch {
    throw badRequest('Invalid JSON body');
  }
}

function bucketId(c: Context): string {
  const id = c.req.param('id') ?? '';
  if (!BUCKET_ID_RE.test(id)) throw notFound('Bucket not found');
  return id;
}

export function createApp(getServices: ServicesProvider): Hono<Env> {
  const app = new Hono<Env>();

  app.use('*', async (c, next) => {
    const requestId = crypto.randomUUID();
    c.set('requestId', requestId);
    const started = Date.now();
    c.set('services', await getServices(c));
    await next();
    c.header('x-request-id', requestId);
    c.header('x-content-type-options', 'nosniff');
    c.header('referrer-policy', 'no-referrer');
    c.header('x-frame-options', 'DENY');
    if (!c.res.headers.has('cache-control')) c.header('cache-control', 'no-store');
    log({
      level: 'info',
      msg: 'request',
      requestId,
      method: c.req.method,
      route: c.req.routePath,
      status: c.res.status,
      ms: Date.now() - started,
      userId: c.get('viewer')?.user.id ?? null,
    });
  });

  app.onError((err, c) => {
    if (err instanceof RegistryError) {
      return c.json({ error: { code: err.code, message: err.message, ...(err.details ? { details: err.details } : {}) } }, err.status as 400);
    }
    log({ level: 'error', msg: 'unhandled error', requestId: c.get('requestId'), err: String(err), stack: err.stack });
    return c.json({ error: { code: 'internal', message: 'Internal error' } }, 500);
  });

  app.notFound((c) => c.json({ error: { code: 'not_found', message: 'Not found' } }, 404));

  app.use('*', bodyLimit({ maxSize: 64 * 1024, onError: (c) => c.json({ error: { code: 'bad_request', message: 'Body too large' } }, 413) }));

  // ---------------------------------------------------------------------------
  // Public endpoints
  // ---------------------------------------------------------------------------

  app.get('/health', (c) => {
    const s = c.get('services');
    return c.json({ status: 'ok', runtime: s.runtime, environment: s.config.environment, authMode: s.config.authMode });
  });

  app.get('/', (c) => {
    c.header(
      'content-security-policy',
      "default-src 'none'; script-src 'self'; connect-src 'self'; img-src 'self' https://avatars.githubusercontent.com; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
    );
    return c.html(indexHtml(c.get('services').config.basePath));
  });
  app.get('/app.js', (c) => c.body(APP_JS, 200, { 'content-type': 'text/javascript; charset=utf-8' }));

  app.get('/login/github', async (c) => {
    const s = c.get('services');
    if (s.config.authMode === 'mock') {
      const links = MOCK_USERS.map((u) => `<li><a href="${s.config.basePath}/login/mock?user=${u.login}">${u.login}</a></li>`).join('');
      return c.html(`<!doctype html><title>Mock login</title><p>Development mock login (ENVIRONMENT=local only):</p><ul>${links}</ul>`);
    }
    const { location, cookie } = await s.auth.startLogin(c.req.query('redirect_to'));
    c.header('set-cookie', cookie);
    return c.redirect(location, 302);
  });

  app.get('/login/github/callback', async (c) => {
    const s = c.get('services');
    const r = await s.auth.finishLogin(
      { code: c.req.query('code'), state: c.req.query('state'), error: c.req.query('error') },
      getCookie(c, s.auth.stateCookie),
    );
    for (const ck of r.cookies) c.header('set-cookie', ck, { append: true });
    return c.redirect(`${s.config.basePath}${r.redirect}`, 302);
  });

  app.get('/login/mock', async (c) => {
    const s = c.get('services');
    // Defense in depth: loadConfig already refuses mock mode outside local.
    if (s.config.authMode !== 'mock' || s.config.environment !== 'local') throw notFound();
    const login = c.req.query('user') ?? '';
    c.header('set-cookie', await s.auth.createSession(`mock:${login}`));
    return c.redirect(`${s.config.basePath}/`, 302);
  });

  // ---------------------------------------------------------------------------
  // Authenticated endpoints
  // ---------------------------------------------------------------------------

  const requireAuth = async (c: Context<Env>, next: () => Promise<void>) => {
    const s = c.get('services');
    const r = await s.auth.authenticate({
      authorization: c.req.header('authorization'),
      sessionCookie: getCookie(c, s.auth.sessionCookie),
    });
    if (!r) throw unauthenticated();
    c.set('viewer', r.viewer);
    c.set('session', r.session);
    if (r.session && UNSAFE.has(c.req.method)) {
      s.auth.checkCsrf(r.session, c.req.header('origin'), c.req.header('x-csrf-token'));
    }
    await next();
  };

  app.post('/logout', async (c) => {
    const s = c.get('services');
    const raw = getCookie(c, s.auth.sessionCookie);
    const r = await s.auth.authenticate({ sessionCookie: raw });
    if (r?.session) s.auth.checkCsrf(r.session, c.req.header('origin'), c.req.header('x-csrf-token'));
    c.header('set-cookie', await s.auth.logout(r ? raw : undefined));
    return c.body(null, 204);
  });

  app.use('/v1/*', requireAuth);

  app.get('/v1/me', async (c) => {
    const s = c.get('services');
    const viewer = c.get('viewer');
    let memberships: { orgIds: number[]; teamIds: number[] } | null = null;
    let membershipError: string | null = null;
    try {
      memberships = await s.core.memberships(viewer);
    } catch (e) {
      membershipError = e instanceof RegistryError ? e.message : 'unavailable';
    }
    const u = viewer.user;
    return c.json({
      user: { id: u.id, login: u.login, name: u.name, avatarUrl: u.avatarUrl },
      authMode: s.config.authMode,
      csrfToken: c.get('session')?.csrfToken ?? null,
      memberships,
      membershipError,
    });
  });

  app.get('/v1/buckets', async (c) => c.json({ buckets: await c.get('services').core.listBuckets(c.get('viewer')) }));

  app.post('/v1/buckets', async (c) => {
    const view = await c.get('services').core.createBucket(c.get('viewer'), await jsonBody(c));
    return c.json(view, 201);
  });

  app.get('/v1/buckets/:id', async (c) => c.json(await c.get('services').core.getBucket(c.get('viewer'), bucketId(c))));

  app.patch('/v1/buckets/:id', async (c) =>
    c.json(await c.get('services').core.updateBucket(c.get('viewer'), bucketId(c), await jsonBody(c))),
  );

  app.delete('/v1/buckets/:id', async (c) => {
    await c.get('services').core.deleteBucket(c.get('viewer'), bucketId(c));
    return c.body(null, 204);
  });

  app.get('/v1/buckets/:id/acl', async (c) =>
    c.json({ entries: await c.get('services').core.getAcl(c.get('viewer'), bucketId(c)) }),
  );

  app.put('/v1/buckets/:id/acl', async (c) =>
    c.json({ entries: await c.get('services').core.putAcl(c.get('viewer'), bucketId(c), await jsonBody(c)) }),
  );

  app.post('/v1/buckets/:id/verify', async (c) => c.json(await c.get('services').core.verify(c.get('viewer'), bucketId(c))));

  app.put('/v1/buckets/:id/credentials', async (c) =>
    c.json(await c.get('services').core.putCredentials(c.get('viewer'), bucketId(c), await jsonBody(c))),
  );

  app.get('/v1/buckets/:id/objects', async (c) => {
    const q = c.req.query();
    return c.json(
      await c.get('services').core.listObjects(c.get('viewer'), bucketId(c), {
        prefix: q.prefix,
        cursor: q.cursor,
        limit: q.limit,
        delimiter: q.delimiter,
      }),
    );
  });

  // Keys may contain "/": either percent-encode each segment (a/b c.tar -> a/b%20c.tar)
  // or the whole key as one segment (a%2Fb%20c.tar).
  app.get('/v1/buckets/:id/objects/:key{.+}/download', async (c) => {
    const url = await c.get('services').core.download(c.get('viewer'), bucketId(c), c.req.param('key'));
    if (c.req.query('redirect') === '1') return c.redirect(url.url, 302);
    return c.json(url);
  });

  return app;
}
