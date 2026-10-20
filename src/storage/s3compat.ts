// StorageProvider over the S3 API, shared by Cloudflare R2, AWS S3 and local SeaweedFS.
// Endpoints are derived from validated config only (see validation.ts) — never from user URLs.
import { AwsClient } from 'aws4fetch';
import { RegistryError, badRequest } from '../core/errors.ts';
import { AWS_REGION_RE, BUCKET_RE, R2_ACCOUNT_RE } from '../core/validation.ts';
import type {
  Clock,
  CredentialResolver,
  ObjectPage,
  PresignedUrl,
  StorageFactory,
  StorageProvider,
  VerifyReport,
} from '../core/ports.ts';
import type { Bucket, ResolvedCredentials, StorageConfig } from '../core/types.ts';
import { encodeKey } from '../util/s3key.ts';
import { tagBlocks, tagText } from '../util/xml.ts';

export { encodeKey };

export interface Endpoint {
  /** Base URL of the bucket for server-side calls, without trailing slash. */
  bucketUrl: string;
  /** Base URL of the bucket embedded in presigned URLs handed to clients. */
  publicBucketUrl: string;
  /** Service root (used to detect over-privileged credentials via ListBuckets). */
  serviceUrl: string;
  region: string;
}

export interface LocalStorageSettings {
  /** e.g. http://127.0.0.1:8333 — reachable from the registry runtime. */
  endpoint: string;
  /** e.g. http://localhost:8333 — reachable from the client that downloads. */
  publicEndpoint: string;
}

export function endpointFor(cfg: StorageConfig, local: LocalStorageSettings | null): Endpoint {
  // Re-validate here as well: this is the last line before a hostname is built.
  if (!BUCKET_RE.test(cfg.bucket)) throw badRequest('invalid bucket name');
  switch (cfg.provider) {
    case 'r2': {
      if (!R2_ACCOUNT_RE.test(cfg.accountId)) throw badRequest('invalid R2 account id');
      const svc = `https://${cfg.accountId}.r2.cloudflarestorage.com`;
      return { bucketUrl: `${svc}/${cfg.bucket}`, publicBucketUrl: `${svc}/${cfg.bucket}`, serviceUrl: `${svc}/`, region: 'auto' };
    }
    case 's3': {
      if (!AWS_REGION_RE.test(cfg.region)) throw badRequest('invalid AWS region');
      const b = `https://${cfg.bucket}.s3.${cfg.region}.amazonaws.com`;
      return { bucketUrl: b, publicBucketUrl: b, serviceUrl: `https://s3.${cfg.region}.amazonaws.com/`, region: cfg.region };
    }
    case 'local': {
      if (!local) throw badRequest('local storage is disabled');
      return {
        bucketUrl: `${local.endpoint}/${cfg.bucket}`,
        publicBucketUrl: `${local.publicEndpoint}/${cfg.bucket}`,
        serviceUrl: `${local.endpoint}/`,
        region: 'us-east-1',
      };
    }
  }
}

export class S3CompatibleStorage implements StorageProvider {
  private readonly client: AwsClient;

  constructor(
    private readonly ep: Endpoint,
    creds: ResolvedCredentials,
    private readonly clock: Clock,
    private readonly fetchImpl: typeof fetch = (...a) => fetch(...a),
  ) {
    this.client = new AwsClient({
      accessKeyId: creds.accessKeyId,
      secretAccessKey: creds.secretAccessKey,
      sessionToken: creds.sessionToken,
      service: 's3',
      region: ep.region,
      retries: 0,
    });
  }

  private async send(url: string, init: RequestInit = {}): Promise<Response> {
    const req = await this.client.sign(url, init);
    try {
      return await this.fetchImpl(req);
    } catch {
      throw new RegistryError('storage_error', 'Storage endpoint is unreachable');
    }
  }

  private static async fail(res: Response, op: string): Promise<never> {
    const body = await res.text().catch(() => '');
    const code = tagText(body, 'Code') ?? `HTTP ${res.status}`;
    throw new RegistryError('storage_error', `${op} failed: ${code}`, { storageStatus: res.status, storageCode: code });
  }

  async list(opts: { prefix?: string; cursor?: string; limit: number; delimiter?: string }): Promise<ObjectPage> {
    const q = new URLSearchParams({ 'list-type': '2', 'max-keys': String(opts.limit) });
    if (opts.prefix) q.set('prefix', opts.prefix);
    if (opts.cursor) q.set('continuation-token', opts.cursor);
    if (opts.delimiter) q.set('delimiter', opts.delimiter);
    const res = await this.send(`${this.ep.bucketUrl}/?${q}`);
    if (!res.ok) return S3CompatibleStorage.fail(res, 'ListObjectsV2');
    return parseListObjectsV2(await res.text());
  }

  async head(key: string): Promise<{ size: number; etag: string } | null> {
    const res = await this.send(`${this.ep.bucketUrl}/${encodeKey(key)}`, { method: 'HEAD' });
    if (res.status === 404) return null;
    if (!res.ok) return S3CompatibleStorage.fail(res, 'HeadObject');
    return { size: Number(res.headers.get('content-length') ?? 0), etag: res.headers.get('etag') ?? '' };
  }

  async presignGet(key: string, ttlSeconds: number): Promise<PresignedUrl> {
    const url = new URL(`${this.ep.publicBucketUrl}/${encodeKey(key)}`);
    url.searchParams.set('X-Amz-Expires', String(ttlSeconds));
    const signed = await this.client.sign(url.toString(), { method: 'GET', aws: { signQuery: true } });
    return { url: signed.url, method: 'GET', expiresAt: new Date(this.clock.now() + ttlSeconds * 1000).toISOString() };
  }

  async verify({ expectPrivate }: { expectPrivate: boolean }): Promise<VerifyReport> {
    const checks: VerifyReport['checks'] = [];
    const warnings: string[] = [];
    const probe = async <T>(name: string, fn: () => Promise<T>): Promise<T | undefined> => {
      try {
        const v = await fn();
        checks.push({ name, ok: true });
        return v;
      } catch (e) {
        checks.push({ name, ok: false, detail: e instanceof Error ? e.message : String(e) });
        return undefined;
      }
    };
    // Anonymous requests must be rejected, otherwise the bucket ACL could be bypassed.
    const anonymousDenied = async (name: string, url: string, method: string, what: string) => {
      const res = await this.fetchImpl(url, { method }).catch(() => null);
      const open = res?.status === 200;
      checks.push({ name, ok: !open, detail: open ? `${what} publicly readable; disable public access` : undefined });
    };

    const page = await probe('list_objects', () => this.list({ limit: 1 }));
    if (!page) return { ok: false, checks, warnings };
    const sampleKey = page.objects[0]?.key ?? null;
    await probe('head_object', () => this.head(sampleKey ?? `.ccpr-verify-${this.clock.now()}`));

    if (expectPrivate) {
      await anonymousDenied('anonymous_list_denied', `${this.ep.bucketUrl}/?list-type=2&max-keys=1`, 'GET', 'bucket listing is');
      if (sampleKey) await anonymousDenied('anonymous_read_denied', `${this.ep.bucketUrl}/${encodeKey(sampleKey)}`, 'HEAD', 'objects are');
      warnings.push(
        'The registry cannot detect public access through r2.dev, custom domains or CDNs in front of the bucket; keep those disabled for private buckets.',
      );
    }

    // Least privilege: bucket-scoped credentials must not be able to list all buckets.
    const lb = await this.send(this.ep.serviceUrl).catch(() => null);
    if (lb?.status === 200) {
      warnings.push('Credentials can list all buckets in the account; use a token/role scoped to this bucket only.');
    }

    return { ok: checks.every((c) => c.ok), checks, warnings };
  }
}

export function parseListObjectsV2(xml: string): ObjectPage {
  const objects = tagBlocks(xml, 'Contents').map((c) => ({
    key: tagText(c, 'Key') ?? '',
    size: Number(tagText(c, 'Size') ?? 0),
    etag: tagText(c, 'ETag') ?? '',
    lastModified: tagText(c, 'LastModified') ?? '',
  }));
  const prefixes = tagBlocks(xml, 'CommonPrefixes').map((c) => tagText(c, 'Prefix') ?? '');
  const truncated = tagText(xml, 'IsTruncated') === 'true';
  return { objects, prefixes, nextCursor: truncated ? tagText(xml, 'NextContinuationToken') : null };
}

export class S3CompatibleStorageFactory implements StorageFactory {
  constructor(
    private readonly credentials: CredentialResolver,
    private readonly local: LocalStorageSettings | null,
    private readonly clock: Clock,
    private readonly fetchImpl?: typeof fetch,
  ) {}

  validateConfig(cfg: StorageConfig): void {
    endpointFor(cfg, this.local);
  }

  async forBucket(bucket: Bucket): Promise<StorageProvider> {
    const ep = endpointFor(bucket.storage, this.local);
    const creds = await this.credentials.resolve(bucket);
    return new S3CompatibleStorage(ep, creds, this.clock, this.fetchImpl);
  }
}
