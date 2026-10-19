// Runtime configuration shared by Cloudflare and AWS entry points.
// Fails closed: an unsafe combination (e.g. mock auth on a public deployment) throws.

export type Environment = 'local' | 'staging' | 'production';
export type AuthMode = 'github' | 'mock';

export interface RegistryConfig {
  environment: Environment;
  authMode: AuthMode;
  publicBaseUrl: string;
  github: { clientId: string; clientSecret: string } | null;
  credentialKeys: string;
  presignTtlSeconds: number;
  membershipCacheSeconds: number;
  sessionTtlSeconds: number;
  local: { endpoint: string; publicEndpoint: string } | null;
  aws: { principalArn: string | null; stsRegion: string };
  auditRetentionDays: number;
}

export type RawEnv = Record<string, string | undefined>;

const PRESIGN_TTL_MAX = 300;

function int(v: string | undefined, def: number, min: number, max: number, name: string): number {
  if (v === undefined || v === '') return def;
  const n = Number(v);
  if (!Number.isInteger(n) || n < min || n > max) throw new Error(`${name} must be an integer in [${min}, ${max}]`);
  return n;
}

export function loadConfig(env: RawEnv): RegistryConfig {
  const environment = (env.ENVIRONMENT ?? '') as Environment;
  if (!['local', 'staging', 'production'].includes(environment)) {
    throw new Error('ENVIRONMENT must be one of local|staging|production');
  }
  const authMode = (env.AUTH_MODE ?? 'github') as AuthMode;
  if (authMode !== 'github' && authMode !== 'mock') throw new Error('AUTH_MODE must be github|mock');
  if (authMode === 'mock' && environment !== 'local') {
    throw new Error('AUTH_MODE=mock is forbidden outside ENVIRONMENT=local');
  }

  const publicBaseUrl = (env.PUBLIC_BASE_URL ?? '').replace(/\/+$/, '');
  let base: URL;
  try {
    base = new URL(publicBaseUrl);
  } catch {
    throw new Error('PUBLIC_BASE_URL must be an absolute URL');
  }
  if (environment !== 'local' && base.protocol !== 'https:') throw new Error('PUBLIC_BASE_URL must be https outside local');

  let github: RegistryConfig['github'] = null;
  if (authMode === 'github') {
    if (!env.GITHUB_CLIENT_ID || !env.GITHUB_CLIENT_SECRET) throw new Error('GITHUB_CLIENT_ID / GITHUB_CLIENT_SECRET are required');
    github = { clientId: env.GITHUB_CLIENT_ID, clientSecret: env.GITHUB_CLIENT_SECRET };
  }

  if (!env.CREDENTIAL_KEYS) throw new Error('CREDENTIAL_KEYS is required');

  let local: RegistryConfig['local'] = null;
  if (env.LOCAL_S3_ENDPOINT) {
    if (environment !== 'local') throw new Error('LOCAL_S3_ENDPOINT is only allowed with ENVIRONMENT=local');
    local = {
      endpoint: env.LOCAL_S3_ENDPOINT.replace(/\/+$/, ''),
      publicEndpoint: (env.LOCAL_S3_PUBLIC_ENDPOINT ?? env.LOCAL_S3_ENDPOINT).replace(/\/+$/, ''),
    };
  }

  return {
    environment,
    authMode,
    publicBaseUrl,
    github,
    credentialKeys: env.CREDENTIAL_KEYS,
    presignTtlSeconds: int(env.PRESIGN_TTL_SECONDS, 300, 30, PRESIGN_TTL_MAX, 'PRESIGN_TTL_SECONDS'),
    membershipCacheSeconds: int(env.MEMBERSHIP_CACHE_SECONDS, 300, 0, 3600, 'MEMBERSHIP_CACHE_SECONDS'),
    sessionTtlSeconds: int(env.SESSION_TTL_SECONDS, 8 * 3600, 300, 7 * 86400, 'SESSION_TTL_SECONDS'),
    local,
    aws: { principalArn: env.REGISTRY_AWS_PRINCIPAL_ARN || null, stsRegion: env.AWS_STS_REGION || 'us-east-1' },
    auditRetentionDays: int(env.AUDIT_RETENTION_DAYS, 400, 1, 3650, 'AUDIT_RETENTION_DAYS'),
  };
}
