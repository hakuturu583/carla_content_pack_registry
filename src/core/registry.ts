// RegistryCore: bucket registration, ACL, authorization and download orchestration.
// Runtime-agnostic: everything platform-specific arrives through the ports.
import { canManage, decideRead, decideWithoutMemberships } from './authz.ts';
import { RegistryError, badRequest, forbidden, notFound } from './errors.ts';
import type {
  Clock,
  CredentialResolver,
  IdentityProvider,
  ObjectPage,
  PresignedUrl,
  PrincipalRef,
  RateLimiter,
  RegistryRepository,
  StorageFactory,
  VerifyReport,
} from './ports.ts';
import type { AclEntry, Bucket, Memberships, StorageConfig, User } from './types.ts';
import {
  LIMITS,
  asObject,
  parseAclInput,
  parseStaticCredentials,
  parseStorageInput,
  readString,
  validateDescription,
  validateName,
  validateObjectKey,
  validatePrefix,
  validateVisibility,
} from './validation.ts';

export interface Viewer {
  user: User;
  /** GitHub access token (decrypted, in memory only). */
  token: string;
}

export interface RegistryOptions {
  presignTtlSeconds: number;
  membershipCacheSeconds: number;
  allowLocalStorage: boolean;
  /** ARN of the IAM principal that calls sts:AssumeRole (shown to S3 bucket owners). */
  awsPrincipalArn: string | null;
}

export interface RegistryDeps {
  repo: RegistryRepository;
  identity: IdentityProvider;
  storage: StorageFactory;
  credentials: CredentialResolver;
  rateLimiter: RateLimiter;
  clock: Clock;
  newId: () => string;
  log: (event: Record<string, unknown>) => void;
}

/** Public projection of a bucket. Never contains credential material. */
export interface BucketView {
  id: string;
  owner: { id: number; login: string | null };
  name: string;
  description: string;
  visibility: Bucket['visibility'];
  provider: Bucket['storage']['provider'];
  verifyStatus: Bucket['verifyStatus'];
  verifiedAt: string | null;
  createdAt: string;
  updatedAt: string;
  permissions: { read: true; manage: boolean };
  /** Owner-only details. */
  storage?: Record<string, string>;
  hasCredentials?: boolean;
  verifyDetail?: string | null;
  awsTrust?: { principalArn: string | null; externalId: string };
}

export class RegistryCore {
  private readonly membershipMemo = new WeakMap<Viewer, Promise<Memberships>>();

  constructor(
    private readonly deps: RegistryDeps,
    private readonly opts: RegistryOptions,
  ) {}

  // -------------------------------------------------------------------------
  // Identity helpers
  // -------------------------------------------------------------------------

  async memberships(viewer: Viewer): Promise<Memberships> {
    let p = this.membershipMemo.get(viewer);
    if (!p) {
      p = this.loadMemberships(viewer);
      this.membershipMemo.set(viewer, p);
    }
    return p;
  }

  private async loadMemberships(viewer: Viewer): Promise<Memberships> {
    const now = this.deps.clock.now();
    const cached = await this.deps.repo.getCachedMemberships(viewer.user.id);
    if (cached && cached.expiresAt > now) return cached.value;
    // Any failure here propagates as membership_unverifiable => access denied.
    const fresh = await this.deps.identity.getMemberships(viewer.token);
    await this.deps.repo.putCachedMemberships(viewer.user.id, fresh, now + this.opts.membershipCacheSeconds * 1000);
    return fresh;
  }

  private async principalsOf(viewer: Viewer): Promise<PrincipalRef[]> {
    const refs: PrincipalRef[] = [{ type: 'user', id: viewer.user.id }];
    try {
      const m = await this.memberships(viewer);
      refs.push(...m.orgIds.map((id) => ({ type: 'org' as const, id })));
      refs.push(...m.teamIds.map((id) => ({ type: 'team' as const, id })));
    } catch (e) {
      // Listing degrades to user/public/owned buckets; org/team grants are denied.
      this.deps.log({ level: 'warn', msg: 'memberships unavailable for listing', userId: viewer.user.id, err: String(e) });
    }
    return refs;
  }

  // -------------------------------------------------------------------------
  // Authorization
  // -------------------------------------------------------------------------

  /** Loads a bucket the viewer may read. Unreadable private buckets look like 404. */
  private async readableBucket(viewer: Viewer, id: string, action: string): Promise<{ bucket: Bucket; acl: AclEntry[] }> {
    const bucket = await this.deps.repo.getBucket(id);
    if (!bucket) throw notFound('Bucket not found');
    const acl = await this.deps.repo.getAcl(id);
    let decision = decideWithoutMemberships(bucket, acl, viewer.user.id);
    if (!decision) {
      let m: Memberships;
      try {
        m = await this.memberships(viewer);
      } catch (e) {
        await this.audit(viewer, action, id, 'deny', { reason: 'membership_unverifiable' });
        if (e instanceof RegistryError) throw e;
        throw new RegistryError('membership_unverifiable', 'Could not verify GitHub organization/team membership');
      }
      decision = decideRead(bucket, acl, viewer.user.id, m);
    }
    if (!decision.allow) {
      await this.audit(viewer, action, id, 'deny', { reason: decision.reason });
      throw notFound('Bucket not found');
    }
    return { bucket, acl };
  }

  private async ownedBucket(viewer: Viewer, id: string, action: string): Promise<Bucket> {
    const { bucket } = await this.readableBucket(viewer, id, action);
    if (!canManage(bucket, viewer.user.id)) {
      await this.audit(viewer, action, id, 'deny', { reason: 'not_owner' });
      throw forbidden('Only the bucket owner can do this');
    }
    return bucket;
  }

  private async rateLimit(key: string): Promise<void> {
    if (!(await this.deps.rateLimiter.limit(key))) {
      throw new RegistryError('rate_limited', 'Too many requests');
    }
  }

  // -------------------------------------------------------------------------
  // Buckets
  // -------------------------------------------------------------------------

  async listBuckets(viewer: Viewer): Promise<BucketView[]> {
    const principals = await this.principalsOf(viewer);
    const candidates = await this.deps.repo.listCandidateBuckets(viewer.user.id, principals);
    const memberships: Memberships = {
      orgIds: principals.filter((p) => p.type === 'org').map((p) => p.id),
      teamIds: principals.filter((p) => p.type === 'team').map((p) => p.id),
    };
    const out: BucketView[] = [];
    for (const b of candidates) {
      const acl = await this.deps.repo.getAcl(b.id);
      if (decideRead(b, acl, viewer.user.id, memberships).allow) out.push(await this.view(b, viewer));
    }
    out.sort((a, b) => a.createdAt.localeCompare(b.createdAt) || a.id.localeCompare(b.id));
    return out;
  }

  async getBucket(viewer: Viewer, id: string): Promise<BucketView> {
    const { bucket } = await this.readableBucket(viewer, id, 'bucket.read');
    return this.view(bucket, viewer);
  }

  async createBucket(viewer: Viewer, body: unknown): Promise<BucketView> {
    await this.rateLimit(`user:${viewer.user.id}:write`);
    const o = asObject(body);
    const name = validateName(readString(o, 'name', true));
    const description = validateDescription(readString(o, 'description', false) ?? '');
    const visibility = validateVisibility(readString(o, 'visibility', false) ?? 'private');
    const input = parseStorageInput(o.storage, this.opts.allowLocalStorage);
    const id = this.deps.newId();

    let storage: StorageConfig;
    if (input.provider === 'r2') storage = { provider: 'r2', accountId: input.accountId!, bucket: input.bucket };
    else if (input.provider === 's3')
      storage = {
        provider: 's3',
        region: input.region!,
        bucket: input.bucket,
        roleArn: input.roleArn!,
        externalId: `ccpr-${this.deps.newId()}`,
      };
    else storage = { provider: 'local', bucket: input.bucket };
    this.deps.storage.validateConfig(storage);

    let credentialCiphertext: string | null = null;
    if (storage.provider !== 's3') {
      if (o.credentials === undefined) throw badRequest('credentials are required for this provider');
      const creds = parseStaticCredentials(o.credentials);
      credentialCiphertext = await this.deps.credentials.seal(id, storage.provider, creds);
    } else if (o.credentials !== undefined) {
      throw badRequest('S3 buckets use an IAM role; do not send static credentials');
    }

    const ts = new Date(this.deps.clock.now()).toISOString();
    const bucket: Bucket = {
      id,
      ownerId: viewer.user.id,
      name,
      description,
      visibility,
      storage,
      credentialCiphertext,
      verifyStatus: 'unverified',
      verifyDetail: null,
      verifiedAt: null,
      createdAt: ts,
      updatedAt: ts,
    };
    await this.deps.repo.createBucket(bucket);
    await this.audit(viewer, 'bucket.create', id, 'ok', { provider: storage.provider, visibility });
    return this.view(bucket, viewer);
  }

  async updateBucket(viewer: Viewer, id: string, body: unknown): Promise<BucketView> {
    await this.rateLimit(`user:${viewer.user.id}:write`);
    const bucket = await this.ownedBucket(viewer, id, 'bucket.update');
    const o = asObject(body);
    const allowed = new Set(['name', 'description', 'visibility']);
    for (const k of Object.keys(o)) if (!allowed.has(k)) throw badRequest(`field ${k} cannot be updated here`);
    const next: Bucket = { ...bucket };
    if (o.name !== undefined) next.name = validateName(readString(o, 'name', true));
    if (o.description !== undefined) next.description = validateDescription(readString(o, 'description', true));
    if (o.visibility !== undefined) {
      next.visibility = validateVisibility(readString(o, 'visibility', true));
      // public -> private must be re-verified (storage must not be directly public).
      if (bucket.visibility === 'public' && next.visibility === 'private') {
        next.verifyStatus = 'unverified';
        next.verifyDetail = 'visibility changed to private; re-run verification';
      }
    }
    next.updatedAt = new Date(this.deps.clock.now()).toISOString();
    await this.deps.repo.updateBucket(next);
    await this.audit(viewer, 'bucket.update', id, 'ok', { fields: Object.keys(o) });
    return this.view(next, viewer);
  }

  /** Deregisters a bucket. Only metadata is removed; objects in R2/S3 are untouched. */
  async deleteBucket(viewer: Viewer, id: string): Promise<void> {
    await this.ownedBucket(viewer, id, 'bucket.delete');
    await this.deps.repo.deleteBucket(id);
    await this.audit(viewer, 'bucket.delete', id, 'ok');
  }

  async putCredentials(viewer: Viewer, id: string, body: unknown): Promise<BucketView> {
    await this.rateLimit(`user:${viewer.user.id}:write`);
    const bucket = await this.ownedBucket(viewer, id, 'bucket.credentials');
    if (bucket.storage.provider === 's3') {
      const o = asObject(body);
      const roleArn = readString(o, 'roleArn', true);
      const parsed = parseStorageInput({ ...bucket.storage, roleArn }, false);
      const storage = { ...bucket.storage, roleArn: parsed.roleArn! };
      // Rotate the external ID on request so a leaked one can be invalidated.
      if (o.rotateExternalId === true) storage.externalId = `ccpr-${this.deps.newId()}`;
      bucket.storage = storage;
    } else {
      const creds = parseStaticCredentials(body);
      bucket.credentialCiphertext = await this.deps.credentials.seal(id, bucket.storage.provider, creds);
    }
    bucket.verifyStatus = 'unverified';
    bucket.verifyDetail = 'credentials changed; re-run verification';
    bucket.verifiedAt = null;
    bucket.updatedAt = new Date(this.deps.clock.now()).toISOString();
    await this.deps.repo.updateBucket(bucket);
    await this.audit(viewer, 'bucket.credentials', id, 'ok');
    return this.view(bucket, viewer);
  }

  async verify(viewer: Viewer, id: string): Promise<{ bucket: BucketView; report: VerifyReport }> {
    await this.rateLimit(`user:${viewer.user.id}:verify`);
    const bucket = await this.ownedBucket(viewer, id, 'bucket.verify');
    let report: VerifyReport;
    try {
      const sp = await this.deps.storage.forBucket(bucket);
      report = await sp.verify({ expectPrivate: bucket.visibility === 'private' });
    } catch (e) {
      report = {
        ok: false,
        checks: [{ name: 'credentials', ok: false, detail: e instanceof RegistryError ? e.message : 'could not obtain credentials' }],
        warnings: [],
      };
      this.deps.log({ level: 'warn', msg: 'verify failed', bucketId: id, err: String(e) });
    }
    const now = new Date(this.deps.clock.now()).toISOString();
    bucket.verifyStatus = report.ok ? 'verified' : 'failed';
    bucket.verifyDetail = report.ok ? null : report.checks.filter((c) => !c.ok).map((c) => c.name).join(', ');
    bucket.verifiedAt = report.ok ? now : null;
    bucket.updatedAt = now;
    await this.deps.repo.updateBucket(bucket);
    await this.audit(viewer, 'bucket.verify', id, report.ok ? 'ok' : 'error', { checks: report.checks.map((c) => `${c.name}:${c.ok}`) });
    return { bucket: await this.view(bucket, viewer), report };
  }

  // -------------------------------------------------------------------------
  // ACL
  // -------------------------------------------------------------------------

  async getAcl(viewer: Viewer, id: string): Promise<AclEntry[]> {
    await this.ownedBucket(viewer, id, 'acl.read');
    return this.deps.repo.getAcl(id);
  }

  async putAcl(viewer: Viewer, id: string, body: unknown): Promise<AclEntry[]> {
    await this.rateLimit(`user:${viewer.user.id}:write`);
    await this.ownedBucket(viewer, id, 'acl.update');
    const refs = parseAclInput(body);
    const entries: AclEntry[] = [];
    const seen = new Set<string>();
    for (const ref of refs) {
      // Resolve names to immutable numeric IDs with the owner's GitHub token.
      const p = await this.deps.identity.resolvePrincipal(viewer.token, ref);
      const k = `${p.type}:${p.id}`;
      if (seen.has(k)) continue;
      seen.add(k);
      entries.push({ type: p.type, id: p.id, name: p.name });
    }
    await this.deps.repo.replaceAcl(id, entries);
    await this.audit(viewer, 'acl.update', id, 'ok', { entries: entries.map((e) => `${e.type}:${e.id}`) });
    return entries;
  }

  // -------------------------------------------------------------------------
  // Objects
  // -------------------------------------------------------------------------

  private requireVerified(bucket: Bucket): void {
    if (bucket.verifyStatus !== 'verified') {
      throw new RegistryError('not_verified', 'Bucket has not passed verification; the owner must run POST /v1/buckets/{id}/verify');
    }
  }

  async listObjects(
    viewer: Viewer,
    id: string,
    q: { prefix?: string; cursor?: string; limit?: string; delimiter?: string },
  ): Promise<ObjectPage> {
    await this.rateLimit(`user:${viewer.user.id}:bucket:${id}`);
    const { bucket } = await this.readableBucket(viewer, id, 'objects.list');
    this.requireVerified(bucket);
    const limit = q.limit === undefined ? 100 : Number(q.limit);
    if (!Number.isInteger(limit) || limit < 1 || limit > LIMITS.listLimitMax) throw badRequest('limit must be 1..1000');
    if (q.delimiter !== undefined && q.delimiter !== '/') throw badRequest('delimiter must be "/"');
    if (q.cursor !== undefined && q.cursor.length > 2048) throw badRequest('cursor is too long');
    const sp = await this.deps.storage.forBucket(bucket);
    return sp.list({ prefix: validatePrefix(q.prefix), cursor: q.cursor, limit, delimiter: q.delimiter });
  }

  async download(viewer: Viewer, id: string, rawKey: string): Promise<PresignedUrl> {
    await this.rateLimit(`user:${viewer.user.id}:bucket:${id}`);
    const key = validateObjectKey(rawKey);
    const { bucket } = await this.readableBucket(viewer, id, 'objects.download');
    this.requireVerified(bucket);
    const sp = await this.deps.storage.forBucket(bucket);
    const head = await sp.head(key);
    if (!head) throw notFound('Object not found');
    const url = await sp.presignGet(key, this.opts.presignTtlSeconds);
    await this.audit(viewer, 'objects.download', id, 'allow', { key, size: head.size });
    return url;
  }

  // -------------------------------------------------------------------------
  // Views / audit
  // -------------------------------------------------------------------------

  private async view(b: Bucket, viewer: Viewer): Promise<BucketView> {
    const owner = b.ownerId === viewer.user.id ? viewer.user : await this.deps.repo.getUser(b.ownerId);
    const isOwner = b.ownerId === viewer.user.id;
    const v: BucketView = {
      id: b.id,
      owner: { id: b.ownerId, login: owner?.login ?? null },
      name: b.name,
      description: b.description,
      visibility: b.visibility,
      provider: b.storage.provider,
      verifyStatus: b.verifyStatus,
      verifiedAt: b.verifiedAt,
      createdAt: b.createdAt,
      updatedAt: b.updatedAt,
      permissions: { read: true, manage: isOwner },
    };
    if (isOwner) {
      const s = b.storage;
      v.storage =
        s.provider === 'r2'
          ? { provider: 'r2', accountId: s.accountId, bucket: s.bucket }
          : s.provider === 's3'
            ? { provider: 's3', region: s.region, bucket: s.bucket, roleArn: s.roleArn }
            : { provider: 'local', bucket: s.bucket };
      v.hasCredentials = s.provider === 's3' ? true : b.credentialCiphertext !== null;
      v.verifyDetail = b.verifyDetail;
      if (s.provider === 's3') v.awsTrust = { principalArn: this.opts.awsPrincipalArn, externalId: s.externalId };
    }
    return v;
  }

  async audit(
    viewer: Viewer | null,
    action: string,
    bucketId: string | null,
    outcome: 'allow' | 'deny' | 'error' | 'ok',
    detail?: Record<string, unknown>,
  ): Promise<void> {
    const event = {
      ts: new Date(this.deps.clock.now()).toISOString(),
      actorId: viewer?.user.id ?? null,
      action,
      bucketId,
      outcome,
      ...(detail ? { detail } : {}),
    };
    this.deps.log({ level: 'info', msg: 'audit', ...event });
    try {
      await this.deps.repo.appendAudit(event);
    } catch (e) {
      this.deps.log({ level: 'error', msg: 'audit write failed', err: String(e) });
    }
  }
}
