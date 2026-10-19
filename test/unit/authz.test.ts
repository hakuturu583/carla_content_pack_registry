import { describe, expect, it } from 'vitest';
import { canManage, decideRead } from '../../src/core/authz.ts';
import type { AclEntry } from '../../src/core/types.ts';

const OWNER = 1;
const VIEWER = 2;
const none = { orgIds: [], teamIds: [] };

describe('decideRead', () => {
  const priv = { ownerId: OWNER, visibility: 'private' as const };
  const pub = { ownerId: OWNER, visibility: 'public' as const };

  it('always allows the owner, even with an empty ACL', () => {
    expect(decideRead(priv, [], OWNER, none)).toEqual({ allow: true, reason: 'owner' });
  });

  it('allows any authenticated user on public buckets', () => {
    expect(decideRead(pub, [], VIEWER, none).allow).toBe(true);
  });

  it('denies private buckets without an explicit grant', () => {
    expect(decideRead(priv, [], VIEWER, { orgIds: [10], teamIds: [20] })).toEqual({ allow: false, reason: 'no_match' });
  });

  const cases: [string, AclEntry[], { orgIds: number[]; teamIds: number[] }, boolean][] = [
    ['user match', [{ type: 'user', id: VIEWER, name: 'v' }], none, true],
    ['user mismatch', [{ type: 'user', id: 99, name: 'x' }], none, false],
    ['org match', [{ type: 'org', id: 10, name: 'o' }], { orgIds: [10], teamIds: [] }, true],
    ['org mismatch', [{ type: 'org', id: 10, name: 'o' }], { orgIds: [11], teamIds: [] }, false],
    ['team match', [{ type: 'team', id: 20, name: 'o/t' }], { orgIds: [10], teamIds: [20] }, true],
    ['org member is not team member', [{ type: 'team', id: 20, name: 'o/t' }], { orgIds: [10], teamIds: [21] }, false],
    // IDs from different namespaces must not be confused (org 20 vs team 20)
    ['type confusion org/team', [{ type: 'team', id: 20, name: 'o/t' }], { orgIds: [20], teamIds: [] }, false],
    ['type confusion user/org', [{ type: 'org', id: VIEWER, name: 'o' }], none, false],
    [
      'any of multiple entries',
      [
        { type: 'user', id: 99, name: 'x' },
        { type: 'team', id: 30, name: 'o/t' },
        { type: 'org', id: 10, name: 'o' },
      ],
      { orgIds: [10], teamIds: [] },
      true,
    ],
  ];
  for (const [name, acl, m, expected] of cases) {
    it(`private: ${name}`, () => expect(decideRead(priv, acl, VIEWER, m).allow).toBe(expected));
  }
});

describe('canManage', () => {
  it('is owner-only', () => {
    expect(canManage({ ownerId: OWNER }, OWNER)).toBe(true);
    expect(canManage({ ownerId: OWNER }, VIEWER)).toBe(false);
  });
});
