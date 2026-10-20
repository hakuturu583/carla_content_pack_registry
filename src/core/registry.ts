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
  LimitClass,
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
  validateRoleArn,
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

  private nowIso(): string {
    return new Date(this.deps.clock.now()).toISOString();
  }

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

  /** Memberships for listing: on failure, degrade to no org/team grants (owned/public/user still listed). */
  private async listingMemberships(viewer: Viewer): Promise<Memberships> {
    try {
      return await this.memberships(viewer);
    } catch (e) {
      this.deps.log({ level: 'warn', msg: 'memberships unavailable for listing', userId: viewer.user.id, err: String(e) });
      return { orgIds: [], teamIds: [] };
    }
  }

  // -------------------------------------------------------------------------
  // Authorization
  // -------------------------------------------------------------------------

  private async loadBucket(id: string): Promise<Bucket> {
    const bucket = await this.deps.repo.getBucket(id);
    if (!bucket) throw notFound('Bucket not found');
    return bucket;
  }

  /** Throws 404 unless the viewer may read the bucket (unreadable private buckets look absent). */
  private async assertReadable(viewer: Viewer, bucket: Bucket, action: string): Promise<void> {
    // Owner and public need no ACL lookup.
    if (canManage(bucket, viewer.user.id) || bucket.visibility === 'public') return;
    const acl = await this.deps.repo.getAcl(bucket.id);
    let decision = decideWithoutMemberships(bucket, acl, viewer.user.id);
    if (!decision) {
      let m: Memberships;
      try {
        m = await this.memberships(viewer);
      } catch (e) {
        await this.audit(viewer, action, bucket.id, 'deny', { reason: 'membership_unverifiable' });
        if (e instanceof RegistryError) throw e;
        throw new RegistryError('membership_unverifiable', 'Could not verify GitHub organization/team membership');
      }
      decision = decideRead(bucket, acl, viewer.user.id, m);
    }
    if (!decision.allow) {
      await this.audit(viewer, action, bucket.id, 'deny', { reason: decision.reason });
      throw notFound('Bucket not found');
    }
  }

  private async readableBucket(viewer: Viewer, id: string, action: string): Promise<Bucket> {
    const bucket = await this.loadBucket(id);
    await this.assertReadable(viewer, bucket, action);
    return bucket;
  }

  /** Owner-only operations: 404 if the viewer cannot even read the bucket, 403 otherwise. */
  private async ownedBucket(viewer: Viewer, id: string, action: string): Promise<Bucket> {
    const bucket = await this.loadBucket(id);
    if (canManage(bucket, viewer.user.id)) return bucket;
    await this.assertReadable(viewer, bucket, action);
    await this.audit(viewer, action, id, 'deny', { reason: 'not_owner' });
    throw forbidden('Only the bucket owner can do this');
  }

  private async rateLimit(cls: LimitClass, key: string): Promise<void> {
    if (!(await this.deps.rateLimiter.limit(cls, key))) {
      throw new RegistryError('rate_limited', 'Too many requests');
    }
  }

  private writeLimit(viewer: Viewer): Promise<void> {
    return this.rateLimit('write', `user:${viewer.user.id}`);
  }

  private readLimit(viewer: Viewer, bucketId: string): Promise<void> {
    return this.rateLimit('read', `user:${viewer.user.id}:bucket:${bucketId}`);
  }

  private markUnverified(b: Bucket, reason: string): void {
    b.verifyStatus = 'unverified';
    b.verifyDetail = reason;
    b.verifiedAt = null;
  }

  // -------------------------------------------------------------------------
  // Buckets
  // -------------------------------------------------------------------------

  async listBuckets(viewer: Viewer): Promise<BucketView[]> {
    const me = viewer.user.id;
    const memberships = await this.listingMemberships(viewer);
    const principals: PrincipalRef[] = [
      { type: 'user', id: me },
      ...memberships.orgIds.map((id) => ({ type: 'org' as const, id })),
      ...memberships.teamIds.map((id) => ({ type: 'team' as const, id })),
    ];
    const candidates = await this.deps.repo.listCandidateBuckets(me, principals);
    // Only private buckets owned by someone else need their ACL checked.
    const readable = await Promise.all(
      candidates.map(async (b) =>
        canManage(b, me) || b.visibility === 'public' || decideRead(b, await this.deps.repo.getAcl(b.id), me, memberships).allow,
      ),
    );
    const visible = candidates.filter((_, i) => readable[i]);
    const ownerIds = [...new Set(visible.map((b) => b.ownerId).filter((id) => id !== me))];
    const owners = new Map<number, User | null>(
      await Promise.all(ownerIds.map(async (id) => [id, await this.deps.repo.getUser(id)] as const)),
    );
    owners.set(me, viewer.user);
    return visible
      .map((b) => this.view(b, viewer, owners.get(b.ownerId)?.login ?? null))
      .sort((a, b) => a.createdAt.localeCompare(b.createdAt) || a.id.localeCompare(b.id));
  }

  async getBucket(viewer: Viewer, id: string): Promise<BucketView> {
    const bucket = await this.readableBucket(viewer, id, 'bucket.read');
    const owner = canManage(bucket, viewer.user.id) ? viewer.user : await this.deps.repo.getUser(bucket.ownerId);
    return this.view(bucket, viewer, owner?.login ?? null);
  }

  async createBucket(viewer: Viewer, body: unknown): Promise<BucketView> {
    await this.writeLimit(viewer);
    const o = asObject(body);
    const name = validateName(readString(o, 'name', true));
    const description = validateDescription(readString(o, 'description', false) ?? '');
    const visibility = validateVisibility(readString(o, 'visibility', false) ?? 'private');
    const input = parseStorageInput(o.storage, this.opts.allowLocalStorage);
    const id = this.deps.newId();
    const storage: StorageConfig = input.provider === 's3' ? { ...input, externalId: `ccpr-${this.deps.newId()}` } : input;
    this.deps.storage.validateConfig(storage);

    let credentialCiphertext: string | null = null;
    if (storage.provider !== 's3') {
      if (o.credentials === undefined) throw badRequest('credentials are required for this provider');
      credentialCiphertext = await this.deps.credentials.seal(id, storage.provider, parseStaticCredentials(o.credentials));
    } else if (o.credentials !== undefined) {
      throw badRequest('S3 buckets use an IAM role; do not send static credentials');
    }

    const ts = this.nowIso();
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
    return this.ownerView(bucket, viewer);
  }

  async updateBucket(viewer: Viewer, id: string, body: unknown): Promise<BucketView> {
    await this.writeLimit(viewer);
    const bucket = await this.ownedBucket(viewer, id, 'bucket.update');
    const o = asObject(body);
    const allowed = new Set(['name', 'description', 'visibility']);
    for (const k of Object.keys(o)) if (!allowed.has(k)) throw badRequest(`field ${k} cannot be updated here`);
    const wasPublic = bucket.visibility === 'public';
    if (o.name !== undefined) bucket.name = validateName(readString(o, 'name', true));
    if (o.description !== undefined) bucket.description = validateDescription(readString(o, 'description', true));
    if (o.visibility !== undefined) bucket.visibility = validateVisibility(readString(o, 'visibility', true));
    // public -> private must be re-verified (storage must not be directly public).
    if (wasPublic && bucket.visibility === 'private') this.markUnverified(bucket, 'visibility changed to private; re-run verification');
    bucket.updatedAt = this.nowIso();
    await this.deps.repo.updateBucket(bucket);
    await this.audit(viewer, 'bucket.update', id, 'ok', { fields: Object.keys(o) });
    return this.ownerView(bucket, viewer);
  }

  /** Deregisters a bucket. Only metadata is removed; objects in R2/S3 are untouched. */
  async deleteBucket(viewer: Viewer, id: string): Promise<void> {
    await this.ownedBucket(viewer, id, 'bucket.delete');
    await this.deps.repo.deleteBucket(id);
    await this.audit(viewer, 'bucket.delete', id, 'ok');
  }

  async putCredentials(viewer: Viewer, id: string, body: unknown): Promise<BucketView> {
    await this.writeLimit(viewer);
    const bucket = await this.ownedBucket(viewer, id, 'bucket.credentials');
    if (bucket.storage.provider === 's3') {
      const o = asObject(body);
      const storage = { ...bucket.storage, roleArn: validateRoleArn(readString(o, 'roleArn', true)) };
      // Rotate the external ID on request so a leaked one can be invalidated.
      if (o.rotateExternalId === true) storage.externalId = `ccpr-${this.deps.newId()}`;
      bucket.storage = storage;
    } else {
      bucket.credentialCiphertext = await this.deps.credentials.seal(id, bucket.storage.provider, parseStaticCredentials(body));
    }
    this.markUnverified(bucket, 'credentials changed; re-run verification');
    bucket.updatedAt = this.nowIso();
    await this.deps.repo.updateBucket(bucket);
    await this.audit(viewer, 'bucket.credentials', id, 'ok');
    return this.ownerView(bucket, viewer);
  }

  async verify(viewer: Viewer, id: string): Promise<{ bucket: BucketView; report: VerifyReport }> {
    await this.rateLimit('write', `user:${viewer.user.id}:verify`);
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
    const now = this.nowIso();
    bucket.verifyStatus = report.ok ? 'verified' : 'failed';
    bucket.verifyDetail = report.ok ? null : report.checks.filter((c) => !c.ok).map((c) => c.name).join(', ');
    bucket.verifiedAt = report.ok ? now : null;
    bucket.updatedAt = now;
    await this.deps.repo.updateBucket(bucket);
    await this.audit(viewer, 'bucket.verify', id, report.ok ? 'ok' : 'error', { checks: report.checks.map((c) => `${c.name}:${c.ok}`) });
    return { bucket: this.ownerView(bucket, viewer), report };
  }

  // -------------------------------------------------------------------------
  // ACL
  // -------------------------------------------------------------------------

  async getAcl(viewer: Viewer, id: string): Promise<AclEntry[]> {
    await this.ownedBucket(viewer, id, 'acl.read');
    return this.deps.repo.getAcl(id);
  }

  async putAcl(viewer: Viewer, id: string, body: unknown): Promise<AclEntry[]> {
    await this.writeLimit(viewer);
    await this.ownedBucket(viewer, id, 'acl.update');
    // Deduplicate by name first so each GitHub lookup happens once, then resolve
    // names to immutable numeric IDs with the owner's token (in parallel).
    const refs = new Map(
      parseAclInput(body).map((r) => [r.type === 'team' ? `team:${r.org}/${r.slug}`.toLowerCase() : `${r.type}:${r.login}`.toLowerCase(), r]),
    );
    const resolved = await Promise.all([...refs.values()].map((ref) => this.deps.identity.resolvePrincipal(viewer.token, ref)));
    const entries = [...new Map(resolved.map((p) => [`${p.type}:${p.id}`, p])).values()];
    await this.deps.repo.replaceAcl(id, entries);
    await this.audit(viewer, 'acl.update', id, 'ok', { entries: entries.map((e) => `${e.type}:${e.id}`) });
    return entries;
  }

  // -------------------------------------------------------------------------
  // Objects
  // -------------------------------------------------------------------------

  private async verifiedReadableBucket(viewer: Viewer, id: string, action: string): Promise<Bucket> {
    const [, bucket] = await Promise.all([this.readLimit(viewer, id), this.readableBucket(viewer, id, action)]);
    if (bucket.verifyStatus !== 'verified') {
      throw new RegistryError('not_verified', 'Bucket has not passed verification; the owner must run POST /v1/buckets/{id}/verify');
    }
    return bucket;
  }

  async listObjects(
    viewer: Viewer,
    id: string,
    q: { prefix?: string; cursor?: string; limit?: string; delimiter?: string },
  ): Promise<ObjectPage> {
    const bucket = await this.verifiedReadableBucket(viewer, id, 'objects.list');
    const limit = q.limit === undefined ? 100 : Number(q.limit);
    if (!Number.isInteger(limit) || limit < 1 || limit > LIMITS.listLimitMax) throw badRequest('limit must be 1..1000');
    if (q.delimiter !== undefined && q.delimiter !== '/') throw badRequest('delimiter must be "/"');
    if (q.cursor !== undefined && q.cursor.length > 2048) throw badRequest('cursor is too long');
    const sp = await this.deps.storage.forBucket(bucket);
    return sp.list({ prefix: validatePrefix(q.prefix), cursor: q.cursor, limit, delimiter: q.delimiter });
  }

  async download(viewer: Viewer, id: string, rawKey: string): Promise<PresignedUrl> {
    const key = validateObjectKey(rawKey);
    const bucket = await this.verifiedReadableBucket(viewer, id, 'objects.download');
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

  private ownerView(b: Bucket, viewer: Viewer): BucketView {
    return this.view(b, viewer, viewer.user.login);
  }

  private view(b: Bucket, viewer: Viewer, ownerLogin: string | null): BucketView {
    const isOwner = canManage(b, viewer.user.id);
    const v: BucketView = {
      id: b.id,
      owner: { id: b.ownerId, login: ownerLogin },
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
      if (s.provider === 's3') {
        const { externalId, ...pub } = s;
        v.storage = pub;
        v.awsTrust = { principalArn: this.opts.awsPrincipalArn, externalId };
      } else {
        v.storage = { ...s };
      }
      v.hasCredentials = s.provider === 's3' || b.credentialCiphertext !== null;
      v.verifyDetail = b.verifyDetail;
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
      ts: this.nowIso(),
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
