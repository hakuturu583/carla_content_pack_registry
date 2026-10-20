// Smoke test against a running registry (wrangler dev, staging, production).
//
//   SMOKE_BASE_URL=http://localhost:8787 SMOKE_TOKEN=mock:alice SMOKE_LOCAL_FLOW=1 npm run smoke
//   SMOKE_BASE_URL=https://registry-staging.example SMOKE_TOKEN=$GH_TOKEN SMOKE_BUCKET_ID=<uuid> npm run smoke
//
// SMOKE_LOCAL_FLOW=1 registers a SeaweedFS bucket with alice's local keys, verifies it,
// and downloads through a presigned URL (local only). Otherwise the script is read-only.
import { LOCAL_KEYS } from './local-fixtures.ts';
const base = (process.env.SMOKE_BASE_URL ?? 'http://localhost:8787').replace(/\/+$/, '');
const token = process.env.SMOKE_TOKEN;
const bucketId = process.env.SMOKE_BUCKET_ID;

let failures = 0;
function check(name: string, ok: boolean, detail = '') {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`);
  if (!ok) failures++;
}

async function call(method: string, path: string, body?: unknown) {
  const headers: Record<string, string> = { accept: 'application/json' };
  if (token) headers.authorization = `Bearer ${token}`;
  if (body !== undefined) headers['content-type'] = 'application/json';
  const r = await fetch(`${base}${path}`, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), redirect: 'manual' });
  const text = await r.text();
  let json: any = null; // eslint-disable-line @typescript-eslint/no-explicit-any
  try {
    json = text ? JSON.parse(text) : null;
  } catch {
    json = text;
  }
  return { status: r.status, json, headers: r.headers };
}

async function downloadFirst(id: string) {
  const objs = await call('GET', `/v1/buckets/${id}/objects?limit=5`);
  check('list objects', objs.status === 200, `status ${objs.status}`);
  const key: string | undefined = objs.json?.objects?.[0]?.key;
  if (!key) return check('bucket has at least one object', false);
  const dl = await call('GET', `/v1/buckets/${id}/objects/${key.split('/').map(encodeURIComponent).join('/')}/download`);
  check('presign download', dl.status === 200 && typeof dl.json?.url === 'string', `status ${dl.status}`);
  if (dl.status !== 200) return;
  const obj = await fetch(dl.json.url);
  check('direct download from storage', obj.status === 200, `status ${obj.status}`);
  check('presigned URL lifetime <= 300s', Date.parse(dl.json.expiresAt) - Date.now() <= 300_000);
}

const health = await call('GET', '/health');
check('GET /health', health.status === 200 && health.json?.status === 'ok', JSON.stringify(health.json));
check('security headers', health.headers.get('x-content-type-options') === 'nosniff');
check('unauthenticated /v1/me is 401', (await fetch(`${base}/v1/me`)).status === 401);

if (token) {
  const me = await call('GET', '/v1/me');
  check('GET /v1/me', me.status === 200, me.json?.user?.login);
  const list = await call('GET', '/v1/buckets');
  check('GET /v1/buckets', list.status === 200, `${list.json?.buckets?.length ?? '?'} buckets`);

  if (process.env.SMOKE_LOCAL_FLOW === '1') {
    const reg = await call('POST', '/v1/buckets', {
      name: `smoke-${Date.now()}`,
      visibility: 'private',
      storage: { provider: 'local', bucket: 'alice-packs' },
      credentials: LOCAL_KEYS.alice,
    });
    check('register bucket', reg.status === 201, `status ${reg.status}`);
    if (reg.status === 201) {
      const v = await call('POST', `/v1/buckets/${reg.json.id}/verify`);
      check('verify bucket', v.json?.report?.ok === true, JSON.stringify(v.json?.report?.checks));
      await downloadFirst(reg.json.id);
      check('deregister bucket', (await call('DELETE', `/v1/buckets/${reg.json.id}`)).status === 204);
    }
  } else if (bucketId) {
    await downloadFirst(bucketId);
  }
}

console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
process.exit(failures ? 1 : 0);
