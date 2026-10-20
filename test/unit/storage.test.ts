import { describe, expect, it } from 'vitest';
import { RegistryError } from '../../src/core/errors.ts';
import { parseStorageInput } from '../../src/core/validation.ts';
import { S3CompatibleStorage, encodeKey, endpointFor, parseListObjectsV2 } from '../../src/storage/s3compat.ts';

const clock = { now: () => Date.parse('2026-01-01T00:00:00Z') };
const ACCOUNT = '0123456789abcdef0123456789abcdef';

describe('endpointFor (SSRF guard)', () => {
  it('derives fixed provider endpoints', () => {
    expect(endpointFor({ provider: 'r2', accountId: ACCOUNT, bucket: 'packs' }, null).bucketUrl).toBe(
      `https://${ACCOUNT}.r2.cloudflarestorage.com/packs`,
    );
    expect(
      endpointFor({ provider: 's3', region: 'eu-west-1', bucket: 'packs', roleArn: 'arn:aws:iam::123456789012:role/r', externalId: 'x' }, null)
        .bucketUrl,
    ).toBe('https://packs.s3.eu-west-1.amazonaws.com');
  });

  it('rejects anything that could redirect requests elsewhere', () => {
    expect(() => endpointFor({ provider: 'r2', accountId: 'evil.com#', bucket: 'packs' }, null)).toThrow(RegistryError);
    expect(() => endpointFor({ provider: 'r2', accountId: ACCOUNT, bucket: 'a/../b' }, null)).toThrow(RegistryError);
    expect(() =>
      endpointFor({ provider: 's3', region: 'x.evil.com/', bucket: 'packs', roleArn: 'arn:aws:iam::123456789012:role/r', externalId: 'x' }, null),
    ).toThrow(RegistryError);
    expect(() => endpointFor({ provider: 'local', bucket: 'packs' }, null)).toThrow(/disabled/);
  });

  it('only accepts "local" storage when enabled', () => {
    expect(() => parseStorageInput({ provider: 'local', bucket: 'packs' }, false)).toThrow(RegistryError);
    expect(parseStorageInput({ provider: 'local', bucket: 'packs' }, true).provider).toBe('local');
  });

  it('validates S3 role ARNs and regions', () => {
    const ok = { provider: 's3', bucket: 'packs', region: 'ap-northeast-1', roleArn: 'arn:aws:iam::123456789012:role/ccpr-read' };
    expect(parseStorageInput(ok, false)).toEqual(ok);
    expect(() => parseStorageInput({ ...ok, roleArn: 'arn:aws:iam::1234:role/x' }, false)).toThrow();
    expect(() => parseStorageInput({ ...ok, region: 'cn-north-1' }, false)).toThrow();
    expect(() => parseStorageInput({ ...ok, bucket: 'has.dots' }, false)).toThrow();
  });
});

describe('encodeKey', () => {
  it('RFC3986-encodes segments and keeps slashes', () => {
    expect(encodeKey("maps/a b(1)!.tar")).toBe('maps/a%20b%281%29%21.tar');
    expect(encodeKey('日本/x+y')).toBe('%E6%97%A5%E6%9C%AC/x%2By');
  });
});

describe('parseListObjectsV2', () => {
  it('parses objects, prefixes, entities and continuation', () => {
    const page = parseListObjectsV2(`<?xml version="1.0"?><ListBucketResult>
      <IsTruncated>true</IsTruncated><NextContinuationToken>tok&amp;1</NextContinuationToken>
      <Contents><Key>a &amp; b.txt</Key><Size>12</Size><ETag>&quot;abc&quot;</ETag><LastModified>2026-01-01T00:00:00Z</LastModified></Contents>
      <CommonPrefixes><Prefix>maps/</Prefix></CommonPrefixes></ListBucketResult>`);
    expect(page).toEqual({
      objects: [{ key: 'a & b.txt', size: 12, etag: '"abc"', lastModified: '2026-01-01T00:00:00Z' }],
      prefixes: ['maps/'],
      nextCursor: 'tok&1',
    });
  });
});

describe('presignGet', () => {
  it('produces a SigV4 query-signed URL with the configured TTL and session token', async () => {
    const ep = endpointFor({ provider: 'r2', accountId: ACCOUNT, bucket: 'packs' }, null);
    const sp = new S3CompatibleStorage(ep, { accessKeyId: 'AKIAEXAMPLE', secretAccessKey: 'secret', sessionToken: 'sess' }, clock);
    const p = await sp.presignGet('maps/Town 10.tar', 300);
    const u = new URL(p.url);
    expect(u.host).toBe(`${ACCOUNT}.r2.cloudflarestorage.com`);
    expect(u.pathname).toBe('/packs/maps/Town%2010.tar');
    expect(u.searchParams.get('X-Amz-Expires')).toBe('300');
    expect(u.searchParams.get('X-Amz-Algorithm')).toBe('AWS4-HMAC-SHA256');
    expect(u.searchParams.get('X-Amz-Credential')).toMatch(/^AKIAEXAMPLE\/\d{8}\/auto\/s3\/aws4_request$/);
    expect(u.searchParams.get('X-Amz-Security-Token')).toBe('sess');
    expect(u.searchParams.get('X-Amz-Signature')).toMatch(/^[0-9a-f]{64}$/);
    expect(p.expiresAt).toBe('2026-01-01T00:05:00.000Z');
    expect(p.url).not.toContain('secret');
  });
});
