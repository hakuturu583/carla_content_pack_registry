// In-memory RegistryRepository for unit tests. Not for deployment.
import type { PrincipalRef, RegistryRepository } from '../core/ports.ts';
import type { AclEntry, AuditEvent, Bucket, GitHubId, Memberships, Session, User } from '../core/types.ts';

const clone = <T>(v: T): T => structuredClone(v);

export class MemoryRepository implements RegistryRepository {
  users = new Map<GitHubId, User>();
  buckets = new Map<string, Bucket>();
  acls = new Map<string, AclEntry[]>();
  sessions = new Map<string, Session>();
  memberships = new Map<GitHubId, { value: Memberships; expiresAt: number }>();
  audit: AuditEvent[] = [];

  async upsertUser(u: Omit<User, 'createdAt' | 'updatedAt'>): Promise<User> {
    const now = new Date().toISOString();
    const prev = this.users.get(u.id);
    const user: User = { ...u, createdAt: prev?.createdAt ?? now, updatedAt: now };
    this.users.set(u.id, user);
    return clone(user);
  }
  async getUser(id: GitHubId) {
    const u = this.users.get(id);
    return u ? clone(u) : null;
  }
  async createBucket(b: Bucket) {
    if (this.buckets.has(b.id)) throw new Error('duplicate bucket id');
    this.buckets.set(b.id, clone(b));
  }
  async getBucket(id: string) {
    const b = this.buckets.get(id);
    return b ? clone(b) : null;
  }
  async updateBucket(b: Bucket) {
    if (!this.buckets.has(b.id)) throw new Error('bucket not found');
    this.buckets.set(b.id, clone(b));
  }
  async deleteBucket(id: string) {
    this.buckets.delete(id);
    this.acls.delete(id);
  }
  async listCandidateBuckets(ownerId: GitHubId, principals: PrincipalRef[]) {
    const keys = new Set(principals.map((p) => `${p.type}:${p.id}`));
    return [...this.buckets.values()]
      .filter(
        (b) =>
          b.ownerId === ownerId ||
          b.visibility === 'public' ||
          (this.acls.get(b.id) ?? []).some((e) => keys.has(`${e.type}:${e.id}`)),
      )
      .map(clone);
  }
  async getAcl(id: string) {
    return clone(this.acls.get(id) ?? []);
  }
  async replaceAcl(id: string, entries: AclEntry[]) {
    this.acls.set(id, clone(entries));
  }
  async putSession(s: Session) {
    this.sessions.set(s.idHash, clone(s));
  }
  async getSession(h: string) {
    const s = this.sessions.get(h);
    return s ? clone(s) : null;
  }
  async deleteSession(h: string) {
    this.sessions.delete(h);
  }
  async getCachedMemberships(id: GitHubId) {
    const m = this.memberships.get(id);
    return m ? clone(m) : null;
  }
  async putCachedMemberships(id: GitHubId, value: Memberships, expiresAt: number) {
    this.memberships.set(id, { value: clone(value), expiresAt });
  }
  async appendAudit(e: AuditEvent) {
    this.audit.push(clone(e));
  }
}
