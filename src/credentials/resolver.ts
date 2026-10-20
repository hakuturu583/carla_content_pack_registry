// CredentialResolver: the only place where storage secrets are decrypted or minted.
//  - R2 / local: bucket-scoped static keys, sealed with AES-GCM on the bucket record.
//  - S3: no stored secret. Short-lived credentials from STS AssumeRole with a
//        registry-generated ExternalId and a session policy limited to read access.
import { AwsClient } from 'aws4fetch';
import { RegistryError } from '../core/errors.ts';
import type { Clock, CredentialResolver } from '../core/ports.ts';
import type { Bucket, ResolvedCredentials, S3Config, StaticCredentials } from '../core/types.ts';
import type { Keyring } from '../util/crypto.ts';
import { tagText } from '../util/xml.ts';

export interface RegistryAwsIdentity {
  /** Credentials of the registry's own IAM principal (IAM user on Cloudflare, Lambda role on AWS). */
  credentials: () => (StaticCredentials & { sessionToken?: string }) | null;
  stsRegion: string;
}

const STS_DURATION_SECONDS = 900; // STS minimum; presigned URLs live <= 5 minutes.
const REFRESH_MARGIN_MS = 6 * 60 * 1000; // never hand out creds that expire before a presigned URL would

function readOnlySessionPolicy(bucket: string): string {
  return JSON.stringify({
    Version: '2012-10-17',
    Statement: [
      { Effect: 'Allow', Action: ['s3:GetObject'], Resource: [`arn:aws:s3:::${bucket}/*`] },
      { Effect: 'Allow', Action: ['s3:ListBucket'], Resource: [`arn:aws:s3:::${bucket}`] },
    ],
  });
}

export class DefaultCredentialResolver implements CredentialResolver {
  // In-flight and resolved AssumeRole calls, so concurrent requests share one STS call.
  private readonly stsCache = new Map<string, Promise<ResolvedCredentials>>();

  constructor(
    private readonly keyring: Keyring,
    private readonly aws: RegistryAwsIdentity | null,
    private readonly clock: Clock,
    private readonly fetchImpl: typeof fetch = (...a) => fetch(...a),
  ) {}

  private aad(bucketId: string, provider: string): string {
    return `bucket:${bucketId}|provider:${provider}`;
  }

  async seal(bucketId: string, provider: string, plaintext: StaticCredentials): Promise<string> {
    return this.keyring.encrypt('storage-credentials', this.aad(bucketId, provider), JSON.stringify(plaintext));
  }

  async resolve(bucket: Bucket): Promise<ResolvedCredentials> {
    if (bucket.storage.provider === 's3') return this.assumeRole(bucket.id, bucket.storage);
    if (!bucket.credentialCiphertext) throw new RegistryError('storage_error', 'Bucket has no credentials configured');
    let parsed: StaticCredentials;
    try {
      parsed = JSON.parse(
        await this.keyring.decrypt('storage-credentials', this.aad(bucket.id, bucket.storage.provider), bucket.credentialCiphertext),
      );
    } catch {
      throw new RegistryError('storage_error', 'Stored credentials could not be decrypted');
    }
    return { accessKeyId: parsed.accessKeyId, secretAccessKey: parsed.secretAccessKey };
  }

  private async assumeRole(bucketId: string, cfg: S3Config): Promise<ResolvedCredentials> {
    const cacheKey = `${bucketId}|${cfg.roleArn}|${cfg.externalId}`;
    const pending = this.stsCache.get(cacheKey);
    if (pending) {
      const hit = await pending.catch(() => null);
      if (hit?.expiresAt && hit.expiresAt - REFRESH_MARGIN_MS > this.clock.now()) return hit;
    }
    const call = this.callAssumeRole(bucketId, cfg);
    this.stsCache.set(cacheKey, call);
    call.catch(() => {
      if (this.stsCache.get(cacheKey) === call) this.stsCache.delete(cacheKey);
    });
    return call;
  }

  private async callAssumeRole(bucketId: string, cfg: S3Config): Promise<ResolvedCredentials> {
    const own = this.aws?.credentials();
    if (!this.aws || !own) throw new RegistryError('misconfigured', 'Registry AWS principal is not configured');
    const client = new AwsClient({ ...own, service: 'sts', region: this.aws.stsRegion });
    const form = new URLSearchParams({
      Action: 'AssumeRole',
      Version: '2011-06-15',
      RoleArn: cfg.roleArn,
      RoleSessionName: `ccpr-${bucketId}`.slice(0, 64),
      ExternalId: cfg.externalId,
      DurationSeconds: String(STS_DURATION_SECONDS),
      Policy: readOnlySessionPolicy(cfg.bucket),
    });
    const req = await client.sign(`https://sts.${this.aws.stsRegion}.amazonaws.com/`, {
      method: 'POST',
      headers: { 'content-type': 'application/x-www-form-urlencoded; charset=utf-8' },
      body: form.toString(),
    });
    const res = await this.fetchImpl(req);
    const text = await res.text();
    if (!res.ok) {
      const code = tagText(text, 'Code') ?? `HTTP ${res.status}`;
      throw new RegistryError('storage_error', `AssumeRole failed (${code}). Check the role trust policy and ExternalId.`);
    }
    const creds: ResolvedCredentials = {
      accessKeyId: tagText(text, 'AccessKeyId') ?? '',
      secretAccessKey: tagText(text, 'SecretAccessKey') ?? '',
      sessionToken: tagText(text, 'SessionToken') ?? '',
      expiresAt: Date.parse(tagText(text, 'Expiration') ?? ''),
    };
    if (!creds.accessKeyId || !creds.secretAccessKey || !creds.sessionToken || Number.isNaN(creds.expiresAt)) {
      throw new RegistryError('storage_error', 'AssumeRole returned an unexpected response');
    }
    return creds;
  }
}
