import { describe, expect, it } from 'vitest';
import { DefaultCredentialResolver } from '../../src/credentials/resolver.ts';
import type { Bucket } from '../../src/core/types.ts';
import { Keyring } from '../../src/util/crypto.ts';

const keyring = Keyring.parse(`k:${Buffer.alloc(32, 3).toString('base64')}`);
let now = Date.parse('2026-01-01T00:00:00Z');
const clock = { now: () => now };

const s3Bucket: Bucket = {
  id: '00000000-0000-4000-8000-000000000001',
  ownerId: 1,
  name: 'b',
  description: '',
  visibility: 'private',
  storage: { provider: 's3', region: 'us-east-1', bucket: 'packs', roleArn: 'arn:aws:iam::123456789012:role/ccpr', externalId: 'ccpr-ext' },
  credentialCiphertext: null,
  verifyStatus: 'verified',
  verifyDetail: null,
  verifiedAt: null,
  createdAt: '',
  updatedAt: '',
};

const STS_OK = `<AssumeRoleResponse><AssumeRoleResult><Credentials>
  <AccessKeyId>ASIATEMP</AccessKeyId><SecretAccessKey>tmpsecret</SecretAccessKey>
  <SessionToken>tmptoken</SessionToken><Expiration>2026-01-01T00:15:00Z</Expiration>
</Credentials></AssumeRoleResult></AssumeRoleResponse>`;

describe('STS AssumeRole', () => {
  it('sends ExternalId and a read-only session policy, and caches the result', async () => {
    const bodies: URLSearchParams[] = [];
    const f = (async (req: Request) => {
      expect(req.url).toBe('https://sts.us-east-1.amazonaws.com/');
      expect(req.headers.get('authorization')).toMatch(/Credential=AKIAREGISTRY\//);
      bodies.push(new URLSearchParams(await req.text()));
      return new Response(STS_OK);
    }) as unknown as typeof fetch;
    const r = new DefaultCredentialResolver(
      keyring,
      { credentials: () => ({ accessKeyId: 'AKIAREGISTRY', secretAccessKey: 'regsecret' }), stsRegion: 'us-east-1' },
      clock,
      f,
    );
    const creds = await r.resolve(s3Bucket);
    expect(creds).toMatchObject({ accessKeyId: 'ASIATEMP', sessionToken: 'tmptoken' });
    const p = bodies[0]!;
    expect(p.get('Action')).toBe('AssumeRole');
    expect(p.get('ExternalId')).toBe('ccpr-ext');
    expect(p.get('RoleArn')).toBe('arn:aws:iam::123456789012:role/ccpr');
    const policy = JSON.parse(p.get('Policy')!);
    expect(JSON.stringify(policy)).toContain('arn:aws:s3:::packs/*');
    const actions = policy.Statement.flatMap((st: { Action: string[] }) => st.Action).sort();
    expect(actions).toEqual(['s3:GetObject', 's3:ListBucket']);

    await r.resolve(s3Bucket);
    expect(bodies).toHaveLength(1); // cached
    now += 10 * 60 * 1000; // within the refresh margin of expiry
    await r.resolve(s3Bucket);
    expect(bodies).toHaveLength(2);
  });

  it('fails clearly when the role trust policy rejects us', async () => {
    const f = (async () =>
      new Response('<ErrorResponse><Error><Code>AccessDenied</Code><Message>not authorized</Message></Error></ErrorResponse>', {
        status: 403,
      })) as unknown as typeof fetch;
    const r = new DefaultCredentialResolver(keyring, { credentials: () => ({ accessKeyId: 'a', secretAccessKey: 'b' }), stsRegion: 'us-east-1' }, clock, f);
    await expect(r.resolve(s3Bucket)).rejects.toMatchObject({ code: 'storage_error', message: expect.stringContaining('AccessDenied') });
  });

  it('is misconfigured without a registry AWS principal', async () => {
    const r = new DefaultCredentialResolver(keyring, { credentials: () => null, stsRegion: 'us-east-1' }, clock);
    await expect(r.resolve(s3Bucket)).rejects.toMatchObject({ code: 'misconfigured' });
  });
});

describe('static credentials', () => {
  it('are sealed per bucket and cannot be replayed onto another bucket', async () => {
    const r = new DefaultCredentialResolver(keyring, null, clock);
    const b: Bucket = { ...s3Bucket, storage: { provider: 'r2', accountId: '0'.repeat(32), bucket: 'packs' } };
    b.credentialCiphertext = await r.seal(b.id, 'r2', { accessKeyId: 'AK', secretAccessKey: 'SK' });
    expect(await r.resolve(b)).toEqual({ accessKeyId: 'AK', secretAccessKey: 'SK' });
    await expect(r.resolve({ ...b, id: '00000000-0000-4000-8000-000000000002' })).rejects.toMatchObject({ code: 'storage_error' });
  });
});
