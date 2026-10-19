// API contract scenarios shared by every runtime under test (in-process memory/D1,
// the Cloudflare Worker entry with D1, and the Lambda handler with DynamoDB).
// Running the same assertions against each is how we check "same API, same authz".
import { beforeAll, describe, expect, it } from 'vitest';
import { LOCAL_KEYS } from './fake-s3.ts';

export interface ApiResponse {
  status: number;
  body: any; // eslint-disable-line @typescript-eslint/no-explicit-any
  headers: Headers;
}

export interface ApiClient {
  call(method: string, path: string, opts?: { as?: string; body?: unknown; headers?: Record<string, string> }): Promise<ApiResponse>;
  /** Fetch a presigned URL the way a client would (directly from storage). */
  fetchUrl(url: string): Promise<{ status: number; text: string }>;
}

const localStorage = (bucket: string) => ({ provider: 'local', bucket });

export function defineApiScenarios(label: string, setup: () => Promise<ApiClient>) {
  describe(`API contract: ${label}`, () => {
    let api: ApiClient;
    const ids: Record<string, string> = {};

    async function register(name: string, visibility: 'public' | 'private', bucket = 'alice-packs', as = 'alice', keys = LOCAL_KEYS.alice) {
      const r = await api.call('POST', '/v1/buckets', {
        as,
        body: { name, visibility, storage: localStorage(bucket), credentials: keys },
      });
      expect(r.status, JSON.stringify(r.body)).toBe(201);
      return r.body.id as string;
    }

    beforeAll(async () => {
      api = await setup();
    });

    it('serves /health without auth', async () => {
      const r = await api.call('GET', '/health');
      expect(r.status).toBe(200);
      expect(r.body.status).toBe('ok');
    });

    it('rejects unauthenticated API calls', async () => {
      expect((await api.call('GET', '/v1/me')).status).toBe(401);
      expect((await api.call('GET', '/v1/buckets')).status).toBe(401);
      expect((await api.call('GET', '/v1/me', { headers: { authorization: 'Bearer mock:mallory' } })).status).toBe(401);
    });

    it('returns the GitHub identity and memberships on /v1/me', async () => {
      const r = await api.call('GET', '/v1/me', { as: 'alice' });
      expect(r.status).toBe(200);
      expect(r.body.user).toMatchObject({ id: 1001, login: 'alice' });
      expect(r.body.memberships.orgIds).toContain(5001);
      expect(r.body.csrfToken).toBeNull(); // bearer auth has no CSRF token
    });

    it('lets alice register buckets without echoing secrets', async () => {
      ids.public = await register('alice-public', 'public');
      ids.private = await register('alice-private', 'private');
      ids.org = await register('alice-org', 'private');
      ids.team = await register('alice-team', 'private');
      ids.user = await register('alice-user-bob', 'private');
      ids.otherlab = await register('alice-otherlab', 'private');
      const r = await api.call('GET', `/v1/buckets/${ids.private}`, { as: 'alice' });
      expect(r.status).toBe(200);
      const text = JSON.stringify(r.body);
      expect(text).not.toContain(LOCAL_KEYS.alice.secretAccessKey);
      expect(text).not.toContain(LOCAL_KEYS.alice.accessKeyId);
      expect(r.body).toMatchObject({ visibility: 'private', verifyStatus: 'unverified', hasCredentials: true, permissions: { manage: true } });
    });

    it('validates registration input (SSRF / provider checks)', async () => {
      const bad = [
        { name: 'x', storage: { provider: 'r2', accountId: 'evil.example.com', bucket: 'b-1' }, credentials: LOCAL_KEYS.alice },
        { name: 'x', storage: { provider: 's3', region: 'us-east-1.evil.com', bucket: 'b-1', roleArn: 'arn:aws:iam::123456789012:role/r' } },
        { name: 'x', storage: { provider: 'http', bucket: 'b-1' } },
        { name: 'x', storage: { provider: 'local', bucket: '../etc' }, credentials: LOCAL_KEYS.alice },
        { name: '', storage: localStorage('alice-packs'), credentials: LOCAL_KEYS.alice },
      ];
      for (const body of bad) expect((await api.call('POST', '/v1/buckets', { as: 'alice', body })).status).toBe(400);
    });

    it('refuses object access before verification', async () => {
      expect((await api.call('GET', `/v1/buckets/${ids.public}/objects`, { as: 'alice' })).status).toBe(409);
    });

    it('lets the owner set ACLs by GitHub name and stores numeric IDs', async () => {
      const set = async (id: string, entries: unknown[]) => {
        const r = await api.call('PUT', `/v1/buckets/${id}/acl`, { as: 'alice', body: { entries } });
        expect(r.status, JSON.stringify(r.body)).toBe(200);
        return r.body.entries;
      };
      expect(await set(ids.org!, [{ type: 'org', login: 'carla-sim' }])).toEqual([{ type: 'org', id: 5001, name: 'carla-sim' }]);
      expect(await set(ids.team!, [{ type: 'team', org: 'carla-sim', slug: 'maps' }])).toEqual([{ type: 'team', id: 6001, name: 'carla-sim/maps' }]);
      expect(await set(ids.user!, [{ type: 'user', login: 'bob' }])).toEqual([{ type: 'user', id: 1002, name: 'bob' }]);
      await set(ids.otherlab!, [{ type: 'org', login: 'other-lab' }, { type: 'user', login: 'carol' }, { type: 'user', login: 'carol' }]);
      const r = await api.call('GET', `/v1/buckets/${ids.otherlab}/acl`, { as: 'alice' });
      expect(r.body.entries).toHaveLength(2); // duplicates collapsed
      expect((await api.call('PUT', `/v1/buckets/${ids.org}/acl`, { as: 'alice', body: { entries: [{ type: 'org', login: 'no-such-org' }] } })).status).toBe(400);
    });

    it('verifies buckets against storage', async () => {
      for (const id of Object.values(ids)) {
        const r = await api.call('POST', `/v1/buckets/${id}/verify`, { as: 'alice' });
        expect(r.status).toBe(200);
        expect(r.body.report.ok, JSON.stringify(r.body.report)).toBe(true);
        expect(r.body.bucket.verifyStatus).toBe('verified');
      }
    });

    it('lists only readable buckets per user (owner / public / user / org / team ACL)', async () => {
      const names = async (as: string) =>
        ((await api.call('GET', '/v1/buckets', { as })).body.buckets as { name: string }[]).map((b) => b.name).sort();
      expect(await names('alice')).toEqual(['alice-org', 'alice-otherlab', 'alice-private', 'alice-public', 'alice-team', 'alice-user-bob']);
      // bob: carla-sim member (vehicles team, not maps), explicitly granted on alice-user-bob
      expect(await names('bob')).toEqual(['alice-org', 'alice-public', 'alice-user-bob']);
      // carol: no orgs, but named on alice-otherlab
      expect(await names('carol')).toEqual(['alice-otherlab', 'alice-public']);
    });

    it('enforces read ACLs on object listing and download', async () => {
      const can = async (as: string, id: string) => (await api.call('GET', `/v1/buckets/${id}/objects`, { as })).status;
      expect(await can('bob', ids.public!)).toBe(200);
      expect(await can('bob', ids.org!)).toBe(200);
      expect(await can('bob', ids.user!)).toBe(200);
      expect(await can('bob', ids.private!)).toBe(404); // existence is not disclosed
      expect(await can('bob', ids.team!)).toBe(404);
      expect(await can('bob', ids.otherlab!)).toBe(404);
      expect(await can('carol', ids.org!)).toBe(404);
      expect(await can('carol', ids.otherlab!)).toBe(200);
      expect(await can('alice', ids.team!)).toBe(200);
    });

    it('lists objects with prefix and pagination', async () => {
      const r = await api.call('GET', `/v1/buckets/${ids.org}/objects?prefix=maps/&limit=1`, { as: 'bob' });
      expect(r.status).toBe(200);
      expect(r.body.objects).toHaveLength(1);
      expect(r.body.objects[0].key).toBe('maps/Town10HD_Opt.tar.zst');
      expect(r.body.nextCursor).toBeTruthy();
      const r2 = await api.call('GET', `/v1/buckets/${ids.org}/objects?prefix=maps/&limit=1&cursor=${encodeURIComponent(r.body.nextCursor)}`, { as: 'bob' });
      expect(r2.body.objects[0].key).toBe('maps/readme.md');
      expect((await api.call('GET', `/v1/buckets/${ids.org}/objects?limit=5000`, { as: 'bob' })).status).toBe(400);
    });

    it('issues short-lived presigned URLs that download directly from storage', async () => {
      const key = 'maps/Town10HD_Opt.tar.zst';
      const r = await api.call('GET', `/v1/buckets/${ids.org}/objects/${encodeURIComponent(key)}/download`, { as: 'bob' });
      expect(r.status, JSON.stringify(r.body)).toBe(200);
      const url = new URL(r.body.url);
      expect(url.searchParams.get('X-Amz-Expires')).toBe('300');
      expect(url.searchParams.get('X-Amz-Signature')).toBeTruthy();
      expect(Date.parse(r.body.expiresAt) - Date.now()).toBeLessThanOrEqual(300_000);
      const obj = await api.fetchUrl(r.body.url);
      expect(obj.status).toBe(200);
      expect(obj.text).toBe('fake map pack: Town10HD_Opt\n');

      // keys with spaces, literal slashes and redirect mode
      const r2 = await api.call('GET', `/v1/buckets/${ids.org}/objects/vehicles/sedan%20with%20space.tar.zst/download?redirect=1`, { as: 'bob' });
      expect(r2.status).toBe(302);
      expect((await api.fetchUrl(r2.headers.get('location')!)).text).toBe('fake vehicle pack\n');

      expect((await api.call('GET', `/v1/buckets/${ids.org}/objects/nope.bin/download`, { as: 'bob' })).status).toBe(404);
      expect((await api.call('GET', `/v1/buckets/${ids.team}/objects/${encodeURIComponent(key)}/download`, { as: 'bob' })).status).toBe(404);
    });

    it('restricts management to the owner', async () => {
      const id = ids.org!; // bob can read it, but not manage it
      expect((await api.call('PATCH', `/v1/buckets/${id}`, { as: 'bob', body: { name: 'pwned' } })).status).toBe(403);
      expect((await api.call('PUT', `/v1/buckets/${id}/acl`, { as: 'bob', body: { entries: [] } })).status).toBe(403);
      expect((await api.call('GET', `/v1/buckets/${id}/acl`, { as: 'bob' })).status).toBe(403);
      expect((await api.call('PUT', `/v1/buckets/${id}/credentials`, { as: 'bob', body: LOCAL_KEYS.bob })).status).toBe(403);
      expect((await api.call('POST', `/v1/buckets/${id}/verify`, { as: 'bob' })).status).toBe(403);
      expect((await api.call('DELETE', `/v1/buckets/${id}`, { as: 'bob' })).status).toBe(403);
      // unreadable bucket: 404 rather than 403
      expect((await api.call('DELETE', `/v1/buckets/${ids.private}`, { as: 'bob' })).status).toBe(404);
      const r = await api.call('GET', `/v1/buckets/${id}`, { as: 'bob' });
      expect(r.body.name).toBe('alice-org');
      expect(r.body.permissions.manage).toBe(false);
      expect(r.body.storage).toBeUndefined(); // storage details are owner-only
    });

    it('lets the owner update settings; going private requires re-verification', async () => {
      const r = await api.call('PATCH', `/v1/buckets/${ids.public}`, { as: 'alice', body: { description: 'maps', visibility: 'private' } });
      expect(r.status).toBe(200);
      expect(r.body).toMatchObject({ description: 'maps', visibility: 'private', verifyStatus: 'unverified' });
      expect((await api.call('GET', `/v1/buckets/${ids.public}/objects`, { as: 'bob' })).status).toBe(404);
      expect((await api.call('PATCH', `/v1/buckets/${ids.public}`, { as: 'alice', body: { ownerId: 1002 } })).status).toBe(400);
      expect((await api.call('POST', `/v1/buckets/${ids.public}/verify`, { as: 'alice' })).body.report.ok).toBe(true);
    });

    it('detects storage that is publicly readable when registered as private', async () => {
      const id = await register('alice-exposed', 'private', 'exposed-packs');
      const r = await api.call('POST', `/v1/buckets/${id}/verify`, { as: 'alice' });
      expect(r.body.report.ok).toBe(false);
      expect(r.body.report.checks.find((c: { name: string }) => c.name === 'anonymous_list_denied').ok).toBe(false);
      expect(r.body.bucket.verifyStatus).toBe('failed');
      expect((await api.call('GET', `/v1/buckets/${id}/objects`, { as: 'alice' })).status).toBe(409);
    });

    it('fails verification when credentials do not grant access (no shared dev key)', async () => {
      // bob tries to register alice's storage bucket with his own keys
      const id = await register('bob-steals', 'public', 'alice-packs', 'bob', LOCAL_KEYS.bob);
      const r = await api.call('POST', `/v1/buckets/${id}/verify`, { as: 'bob' });
      expect(r.body.report.ok).toBe(false);
      expect(r.body.report.checks[0]).toMatchObject({ name: 'list_objects', ok: false });
      // rotating to correct credentials (his own bucket name is fixed, so still denied here)
      const c = await api.call('PUT', `/v1/buckets/${id}/credentials`, { as: 'bob', body: LOCAL_KEYS.bob });
      expect(c.status).toBe(200);
      expect(c.body.verifyStatus).toBe('unverified');
    });

    it('deregisters buckets without touching storage', async () => {
      expect((await api.call('DELETE', `/v1/buckets/${ids.otherlab}`, { as: 'alice' })).status).toBe(204);
      expect((await api.call('GET', `/v1/buckets/${ids.otherlab}`, { as: 'alice' })).status).toBe(404);
      expect((await api.call('GET', `/v1/buckets/${ids.otherlab}/acl`, { as: 'alice' })).status).toBe(404);
      // objects are still served through another registration of the same storage bucket
      const r = await api.call('GET', `/v1/buckets/${ids.org}/objects`, { as: 'alice' });
      expect(r.body.objects.length).toBe(3);
    });

    it('returns 404 for malformed bucket ids and unknown routes', async () => {
      expect((await api.call('GET', '/v1/buckets/not-a-uuid', { as: 'alice' })).status).toBe(404);
      expect((await api.call('GET', '/v1/nope', { as: 'alice' })).status).toBe(404);
    });
  });
}
