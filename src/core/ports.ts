// Ports (interfaces) that isolate RegistryCore from Cloudflare/AWS/GitHub specifics.
import type {
  AclEntry,
  AclRef,
  AuditEvent,
  Bucket,
  GitHubId,
  Memberships,
  ResolvedCredentials,
  StaticCredentials,
  Session,
  StorageConfig,
  User,
} from './types.ts';

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

export type PrincipalRef = Pick<AclEntry, 'type' | 'id'>;

export interface RegistryRepository {
  upsertUser(user: Omit<User, 'createdAt' | 'updatedAt'>): Promise<User>;
  getUser(id: GitHubId): Promise<User | null>;

  createBucket(bucket: Bucket): Promise<void>;
  getBucket(id: string): Promise<Bucket | null>;
  /** Optimistic update: replaces the stored bucket. */
  updateBucket(bucket: Bucket): Promise<void>;
  /** Removes bucket metadata and ACL. Never touches the storage itself. */
  deleteBucket(id: string): Promise<void>;
  /**
   * Candidate buckets that MAY be visible: owned by `ownerId`, public, or with an
   * ACL entry matching one of `principals`. Callers must still run authorization.
   */
  listCandidateBuckets(ownerId: GitHubId, principals: PrincipalRef[]): Promise<Bucket[]>;

  getAcl(bucketId: string): Promise<AclEntry[]>;
  replaceAcl(bucketId: string, entries: AclEntry[]): Promise<void>;

  putSession(session: Session): Promise<void>;
  getSession(idHash: string): Promise<Session | null>;
  deleteSession(idHash: string): Promise<void>;

  getCachedMemberships(userId: GitHubId): Promise<{ value: Memberships; expiresAt: number } | null>;
  putCachedMemberships(userId: GitHubId, value: Memberships, expiresAt: number): Promise<void>;

  appendAudit(event: AuditEvent): Promise<void>;
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

export interface IdentityProfile {
  id: GitHubId;
  login: string;
  name: string | null;
  avatarUrl: string | null;
}

export interface IdentityProvider {
  /** Profile for an access token. Throws RegistryError('unauthenticated') when invalid. */
  getProfile(token: string): Promise<IdentityProfile>;
  /** Active org / team memberships. Throws 'membership_unverifiable' if GitHub can't confirm. */
  getMemberships(token: string): Promise<Memberships>;
  /** Resolve a login/slug to a numeric ID when an owner writes an ACL. */
  resolvePrincipal(token: string, ref: AclRef): Promise<AclEntry>;
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

export interface ObjectSummary {
  key: string;
  size: number;
  etag: string;
  lastModified: string;
}

export interface ObjectPage {
  objects: ObjectSummary[];
  /** Common prefixes when listing with a delimiter. */
  prefixes: string[];
  nextCursor: string | null;
}

export interface PresignedUrl {
  url: string;
  method: 'GET';
  expiresAt: string;
}

export interface VerifyReport {
  ok: boolean;
  checks: { name: string; ok: boolean; detail?: string }[];
  warnings: string[];
}

export interface StorageProvider {
  list(opts: { prefix?: string; cursor?: string; limit: number; delimiter?: string }): Promise<ObjectPage>;
  head(key: string): Promise<{ size: number; etag: string } | null>;
  presignGet(key: string, ttlSeconds: number): Promise<PresignedUrl>;
  verify(opts: { expectPrivate: boolean }): Promise<VerifyReport>;
}

/** Builds a StorageProvider for a bucket using credentials from the CredentialResolver. */
export interface StorageFactory {
  forBucket(bucket: Bucket): Promise<StorageProvider>;
  /** Validates a user-supplied storage config (SSRF guard etc.). Throws RegistryError. */
  validateConfig(cfg: StorageConfig): void;
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

export interface CredentialResolver {
  /** Encrypt user-supplied static credentials for storage on the bucket record. */
  seal(bucketId: string, provider: string, plaintext: StaticCredentials): Promise<string>;
  /** Obtain usable credentials for a bucket (decrypt, or STS AssumeRole). */
  resolve(bucket: Bucket): Promise<ResolvedCredentials>;
}

// ---------------------------------------------------------------------------
// Cross-cutting
// ---------------------------------------------------------------------------

/** "write": management calls per user. "read": object list/download per user and bucket. */
export type LimitClass = 'write' | 'read';

export interface RateLimiter {
  /** Returns true when the request is allowed. */
  limit(cls: LimitClass, key: string): Promise<boolean>;
}

export interface Clock {
  now(): number;
}

export const systemClock: Clock = { now: () => Date.now() };
