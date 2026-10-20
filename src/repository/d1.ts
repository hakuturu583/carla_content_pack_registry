// RegistryRepository on Cloudflare D1 (SQLite). Schema: migrations/0001_init.sql
import type { PrincipalRef, RegistryRepository } from '../core/ports.ts';
import type { AclEntry, AuditEvent, Bucket, GitHubId, Memberships, Session, User } from '../core/types.ts';

type Row = Record<string, unknown>;

const MAX_PRINCIPALS = 90; // keeps the IN-list under D1's bound-parameter limit (100)

function toUser(r: Row): User {
  return {
    id: Number(r.id),
    login: String(r.login),
    name: (r.name as string | null) ?? null,
    avatarUrl: (r.avatar_url as string | null) ?? null,
    createdAt: String(r.created_at),
    updatedAt: String(r.updated_at),
  };
}

function toBucket(r: Row): Bucket {
  return {
    id: String(r.id),
    ownerId: Number(r.owner_id),
    name: String(r.name),
    description: String(r.description ?? ''),
    visibility: r.visibility as Bucket['visibility'],
    storage: JSON.parse(String(r.storage_config)),
    credentialCiphertext: (r.credential_ciphertext as string | null) ?? null,
    verifyStatus: r.verify_status as Bucket['verifyStatus'],
    verifyDetail: (r.verify_detail as string | null) ?? null,
    verifiedAt: (r.verified_at as string | null) ?? null,
    createdAt: String(r.created_at),
    updatedAt: String(r.updated_at),
  };
}

export class D1Repository implements RegistryRepository {
  constructor(private readonly db: D1Database) {}

  async upsertUser(u: Omit<User, 'createdAt' | 'updatedAt'>): Promise<User> {
    const now = new Date().toISOString();
    await this.db
      .prepare(
        `INSERT INTO users (id, login, name, avatar_url, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
         ON CONFLICT(id) DO UPDATE SET login = excluded.login, name = excluded.name, avatar_url = excluded.avatar_url, updated_at = excluded.updated_at`,
      )
      .bind(u.id, u.login, u.name, u.avatarUrl, now)
      .run();
    return (await this.getUser(u.id))!;
  }

  async getUser(id: GitHubId): Promise<User | null> {
    const r = await this.db.prepare('SELECT * FROM users WHERE id = ?1').bind(id).first<Row>();
    return r ? toUser(r) : null;
  }

  private bucketParams(b: Bucket): unknown[] {
    return [
      b.id,
      b.ownerId,
      b.name,
      b.description,
      b.visibility,
      b.storage.provider,
      JSON.stringify(b.storage),
      b.credentialCiphertext,
      b.verifyStatus,
      b.verifyDetail,
      b.verifiedAt,
      b.createdAt,
      b.updatedAt,
    ];
  }

  async createBucket(b: Bucket): Promise<void> {
    await this.db
      .prepare(
        `INSERT INTO buckets (id, owner_id, name, description, visibility, provider, storage_config, credential_ciphertext,
           verify_status, verify_detail, verified_at, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)`,
      )
      .bind(...this.bucketParams(b))
      .run();
  }

  async getBucket(id: string): Promise<Bucket | null> {
    const r = await this.db.prepare('SELECT * FROM buckets WHERE id = ?1').bind(id).first<Row>();
    return r ? toBucket(r) : null;
  }

  async updateBucket(b: Bucket): Promise<void> {
    // owner_id and created_at are immutable (no ownership transfer).
    await this.db
      .prepare(
        `UPDATE buckets SET name = ?3, description = ?4, visibility = ?5, provider = ?6, storage_config = ?7,
           credential_ciphertext = ?8, verify_status = ?9, verify_detail = ?10, verified_at = ?11, updated_at = ?13
         WHERE id = ?1 AND owner_id = ?2 AND created_at = ?12`,
      )
      .bind(...this.bucketParams(b))
      .run();
  }

  async deleteBucket(id: string): Promise<void> {
    await this.db.batch([
      this.db.prepare('DELETE FROM bucket_acl WHERE bucket_id = ?1').bind(id),
      this.db.prepare('DELETE FROM buckets WHERE id = ?1').bind(id),
    ]);
  }

  async listCandidateBuckets(ownerId: GitHubId, principals: PrincipalRef[]): Promise<Bucket[]> {
    const ps = principals.slice(0, MAX_PRINCIPALS);
    const clauses = ps.map((_, i) => `(principal_type = ?${i * 2 + 2} AND principal_id = ?${i * 2 + 3})`).join(' OR ');
    const sql =
      `SELECT * FROM buckets WHERE owner_id = ?1 OR visibility = 'public'` +
      (ps.length ? ` OR id IN (SELECT bucket_id FROM bucket_acl WHERE ${clauses})` : '') +
      ' ORDER BY created_at LIMIT 1000';
    const params: unknown[] = [ownerId];
    for (const p of ps) params.push(p.type, p.id);
    const { results } = await this.db.prepare(sql).bind(...params).all<Row>();
    return results.map(toBucket);
  }

  async getAcl(bucketId: string): Promise<AclEntry[]> {
    const { results } = await this.db
      .prepare('SELECT * FROM bucket_acl WHERE bucket_id = ?1 ORDER BY principal_type, principal_id')
      .bind(bucketId)
      .all<Row>();
    return results.map((r) => ({ type: r.principal_type as AclEntry['type'], id: Number(r.principal_id), name: String(r.principal_name) }));
  }

  async replaceAcl(bucketId: string, entries: AclEntry[]): Promise<void> {
    await this.db.batch([
      this.db.prepare('DELETE FROM bucket_acl WHERE bucket_id = ?1').bind(bucketId),
      ...entries.map((e) =>
        this.db
          .prepare('INSERT INTO bucket_acl (bucket_id, principal_type, principal_id, principal_name) VALUES (?1, ?2, ?3, ?4)')
          .bind(bucketId, e.type, e.id, e.name),
      ),
    ]);
  }

  async putSession(s: Session): Promise<void> {
    await this.db
      .prepare(
        'INSERT OR REPLACE INTO sessions (id_hash, user_id, token_ciphertext, csrf_token, expires_at, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)',
      )
      .bind(s.idHash, s.userId, s.tokenCiphertext, s.csrfToken, s.expiresAt, s.createdAt)
      .run();
  }

  async getSession(idHash: string): Promise<Session | null> {
    const r = await this.db.prepare('SELECT * FROM sessions WHERE id_hash = ?1').bind(idHash).first<Row>();
    if (!r) return null;
    return {
      idHash: String(r.id_hash),
      userId: Number(r.user_id),
      tokenCiphertext: String(r.token_ciphertext),
      csrfToken: String(r.csrf_token),
      expiresAt: Number(r.expires_at),
      createdAt: Number(r.created_at),
    };
  }

  async deleteSession(idHash: string): Promise<void> {
    await this.db.prepare('DELETE FROM sessions WHERE id_hash = ?1').bind(idHash).run();
  }

  async getCachedMemberships(userId: GitHubId): Promise<{ value: Memberships; expiresAt: number } | null> {
    const r = await this.db.prepare('SELECT * FROM membership_cache WHERE user_id = ?1').bind(userId).first<Row>();
    return r ? { value: JSON.parse(String(r.payload)), expiresAt: Number(r.expires_at) } : null;
  }

  async putCachedMemberships(userId: GitHubId, value: Memberships, expiresAt: number): Promise<void> {
    await this.db
      .prepare('INSERT OR REPLACE INTO membership_cache (user_id, payload, expires_at) VALUES (?1, ?2, ?3)')
      .bind(userId, JSON.stringify(value), expiresAt)
      .run();
  }

  async appendAudit(e: AuditEvent): Promise<void> {
    await this.db
      .prepare('INSERT INTO audit_log (ts, actor_id, action, bucket_id, outcome, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6)')
      .bind(e.ts, e.actorId, e.action, e.bucketId, e.outcome, e.detail ? JSON.stringify(e.detail) : null)
      .run();
  }

  /** Housekeeping, run from a scheduled (cron) handler. */
  async purgeExpired(now: number, auditRetentionDays: number): Promise<void> {
    const cutoff = new Date(now - auditRetentionDays * 86400_000).toISOString();
    await this.db.batch([
      this.db.prepare('DELETE FROM sessions WHERE expires_at < ?1').bind(now),
      this.db.prepare('DELETE FROM membership_cache WHERE expires_at < ?1').bind(now),
      this.db.prepare('DELETE FROM audit_log WHERE ts < ?1').bind(cutoff),
    ]);
  }
}
