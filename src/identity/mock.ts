// Development-only identity provider. The runtime refuses to start it outside
// ENVIRONMENT=local (see runtime/config.ts). Tokens look like "mock:<login>".
import { badRequest, unauthenticated } from '../core/errors.ts';
import type { IdentityProfile, IdentityProvider } from '../core/ports.ts';
import type { AclEntry, AclRef, Memberships } from '../core/types.ts';

interface MockUser extends IdentityProfile {
  orgIds: number[];
  teamIds: number[];
}

export const MOCK_ORGS = [
  { id: 5001, login: 'carla-sim' },
  { id: 5002, login: 'other-lab' },
];

export const MOCK_TEAMS = [
  { id: 6001, org: 'carla-sim', slug: 'maps' },
  { id: 6002, org: 'carla-sim', slug: 'vehicles' },
];

/**
 * alice: carla-sim (maps team) + other-lab
 * bob:   carla-sim (vehicles team)
 * carol: no organizations
 */
export const MOCK_USERS: MockUser[] = [
  { id: 1001, login: 'alice', name: 'Alice (mock)', avatarUrl: null, orgIds: [5001, 5002], teamIds: [6001] },
  { id: 1002, login: 'bob', name: 'Bob (mock)', avatarUrl: null, orgIds: [5001], teamIds: [6002] },
  { id: 1003, login: 'carol', name: 'Carol (mock)', avatarUrl: null, orgIds: [], teamIds: [] },
];

export class MockIdentityProvider implements IdentityProvider {
  private user(token: string): MockUser {
    const login = token.startsWith('mock:') ? token.slice(5) : '';
    const u = MOCK_USERS.find((x) => x.login === login);
    if (!u) throw unauthenticated('Unknown mock user');
    return u;
  }

  async getProfile(token: string): Promise<IdentityProfile> {
    const { id, login, name, avatarUrl } = this.user(token);
    return { id, login, name, avatarUrl };
  }

  async getMemberships(token: string): Promise<Memberships> {
    const u = this.user(token);
    return { orgIds: [...u.orgIds], teamIds: [...u.teamIds] };
  }

  async resolvePrincipal(_token: string, ref: AclRef): Promise<AclEntry> {
    if (ref.type === 'user') {
      const u = MOCK_USERS.find((x) => x.login === ref.login);
      if (u) return { type: 'user', id: u.id, name: u.login };
    } else if (ref.type === 'org') {
      const o = MOCK_ORGS.find((x) => x.login === ref.login);
      if (o) return { type: 'org', id: o.id, name: o.login };
    } else {
      const t = MOCK_TEAMS.find((x) => x.org === ref.org && x.slug === ref.slug);
      if (t) return { type: 'team', id: t.id, name: `${t.org}/${t.slug}` };
    }
    throw badRequest(`mock ${ref.type} not found`);
  }
}
