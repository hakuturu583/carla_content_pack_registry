// Pure authorization rules. Kept free of I/O so they can be tested exhaustively.
import type { AclEntry, Bucket, GitHubId, Memberships } from './types.ts';

export type ReadDecision =
  | { allow: true; reason: 'owner' | 'public' | 'acl:user' | 'acl:org' | 'acl:team' }
  | { allow: false; reason: 'no_match' };

/** True when the decision can be made without GitHub org/team membership data. */
export function decideWithoutMemberships(
  bucket: Pick<Bucket, 'ownerId' | 'visibility'>,
  acl: AclEntry[],
  viewerId: GitHubId,
): ReadDecision | null {
  if (bucket.ownerId === viewerId) return { allow: true, reason: 'owner' };
  if (bucket.visibility === 'public') return { allow: true, reason: 'public' };
  if (acl.some((e) => e.type === 'user' && e.id === viewerId)) return { allow: true, reason: 'acl:user' };
  if (!acl.some((e) => e.type === 'org' || e.type === 'team')) return { allow: false, reason: 'no_match' };
  return null;
}

/**
 * Read access: owner always; public buckets for any authenticated user; private
 * buckets only when ANY ACL entry matches (user id, active org membership, or team membership).
 */
export function decideRead(
  bucket: Pick<Bucket, 'ownerId' | 'visibility'>,
  acl: AclEntry[],
  viewerId: GitHubId,
  memberships: Memberships,
): ReadDecision {
  const quick = decideWithoutMemberships(bucket, acl, viewerId);
  if (quick) return quick;
  const orgs = new Set(memberships.orgIds);
  const teams = new Set(memberships.teamIds);
  if (acl.some((e) => e.type === 'org' && orgs.has(e.id))) return { allow: true, reason: 'acl:org' };
  if (acl.some((e) => e.type === 'team' && teams.has(e.id))) return { allow: true, reason: 'acl:team' };
  return { allow: false, reason: 'no_match' };
}

/** Management (ACL, credentials, settings, deregistration) is owner-only. */
export function canManage(bucket: Pick<Bucket, 'ownerId'>, viewerId: GitHubId): boolean {
  return bucket.ownerId === viewerId;
}
