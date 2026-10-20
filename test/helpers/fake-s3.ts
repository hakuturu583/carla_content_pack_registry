// In-process fake of the S3 subset the registry uses, mirroring local/seaweedfs/s3.json.
// It checks *which* access key signed a request (bucket isolation, anonymous access)
// but not the signature itself — real SigV4 is covered by the SeaweedFS integration tests.

import { LOCAL_BUCKETS, LOCAL_KEYS } from '../../scripts/local-fixtures.ts';

export const FAKE_S3_ENDPOINT = 'http://fake-s3.test';

export { LOCAL_KEYS };

interface FakeBucket {
  keys: string[]; // access keys allowed to read/list
  anonymous: boolean;
  objects: Map<string, string>;
}

export function seededBuckets(): Map<string, FakeBucket> {
  return new Map(
    Object.entries(LOCAL_BUCKETS).map(([name, b]) => [
      name,
      { keys: [LOCAL_KEYS[b.owner].accessKeyId], anonymous: b.anonymous, objects: new Map(Object.entries(b.objects)) },
    ]),
  );
}

const xmlEsc = (s: string) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

export function createFakeS3(buckets = seededBuckets()) {
  const requests: { method: string; url: string; accessKey: string | null }[] = [];

  const fakeFetch = async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const req = input instanceof Request ? input : new Request(input, init);
    const url = new URL(req.url);
    if (url.origin !== FAKE_S3_ENDPOINT) return new Response('unexpected host', { status: 599 });
    const auth = req.headers.get('authorization');
    const accessKey = /Credential=([^/]+)\//.exec(auth ?? '')?.[1] ?? url.searchParams.get('X-Amz-Credential')?.split('/')[0] ?? null;
    requests.push({ method: req.method, url: req.url, accessKey });

    const [, bucketName, ...rest] = url.pathname.split('/');
    if (!bucketName) return new Response('<Error><Code>AccessDenied</Code></Error>', { status: 403 });
    const bucket = buckets.get(bucketName);
    if (!bucket) return new Response('<Error><Code>NoSuchBucket</Code></Error>', { status: 404 });
    const allowed = accessKey ? bucket.keys.includes(accessKey) : bucket.anonymous;
    if (!allowed) return new Response('<Error><Code>AccessDenied</Code></Error>', { status: 403 });
    const key = rest.map(decodeURIComponent).join('/');

    if (!key && url.searchParams.get('list-type') === '2') {
      const prefix = url.searchParams.get('prefix') ?? '';
      const max = Number(url.searchParams.get('max-keys') ?? 1000);
      const start = Number(url.searchParams.get('continuation-token') ?? 0);
      const all = [...bucket.objects.keys()].filter((k) => k.startsWith(prefix)).sort();
      const page = all.slice(start, start + max);
      const truncated = start + max < all.length;
      const contents = page
        .map((k) => `<Contents><Key>${xmlEsc(k)}</Key><Size>${bucket.objects.get(k)!.length}</Size><ETag>&quot;e&quot;</ETag><LastModified>2026-01-01T00:00:00.000Z</LastModified></Contents>`)
        .join('');
      return new Response(
        `<?xml version="1.0"?><ListBucketResult><IsTruncated>${truncated}</IsTruncated>${contents}${truncated ? `<NextContinuationToken>${start + max}</NextContinuationToken>` : ''}</ListBucketResult>`,
        { status: 200, headers: { 'content-type': 'application/xml' } },
      );
    }
    const body = bucket.objects.get(key);
    if (body === undefined) return new Response(req.method === 'HEAD' ? null : '<Error><Code>NoSuchKey</Code></Error>', { status: 404 });
    if (req.method === 'HEAD') return new Response(null, { status: 200, headers: { 'content-length': String(body.length), etag: '"e"' } });
    return new Response(body, { status: 200 });
  };

  return { fetch: fakeFetch as typeof fetch, buckets, requests };
}
