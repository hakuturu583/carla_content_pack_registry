// Cloudflare Workers entry point: Workers + D1 (+ Workers Rate Limiting bindings).
import { createApp } from '../http/app.ts';
import { D1Repository } from '../repository/d1.ts';
import { systemClock, type RateLimiter } from '../core/ports.ts';
import { MemoryRateLimiter, limitClass } from '../util/ratelimit.ts';
import { log } from '../util/log.ts';
import { loadConfig, type RawEnv } from './config.ts';
import { buildServices, type Services } from './services.ts';

interface RateLimitBinding {
  limit(opts: { key: string }): Promise<{ success: boolean }>;
}

export interface CloudflareEnv {
  DB: D1Database;
  RL_WRITE?: RateLimitBinding;
  RL_READ?: RateLimitBinding;
  // Vars and secrets (see deploy/cloudflare/wrangler.*.toml):
  ENVIRONMENT?: string;
  AUTH_MODE?: string;
  PUBLIC_BASE_URL?: string;
  GITHUB_CLIENT_ID?: string;
  GITHUB_CLIENT_SECRET?: string;
  CREDENTIAL_KEYS?: string;
  PRESIGN_TTL_SECONDS?: string;
  MEMBERSHIP_CACHE_SECONDS?: string;
  SESSION_TTL_SECONDS?: string;
  LOCAL_S3_ENDPOINT?: string;
  LOCAL_S3_PUBLIC_ENDPOINT?: string;
  REGISTRY_AWS_PRINCIPAL_ARN?: string;
  REGISTRY_AWS_ACCESS_KEY_ID?: string;
  REGISTRY_AWS_SECRET_ACCESS_KEY?: string;
  AWS_STS_REGION?: string;
  AUDIT_RETENTION_DAYS?: string;
}

class CloudflareRateLimiter implements RateLimiter {
  private readonly fallback = new MemoryRateLimiter(systemClock);
  constructor(private readonly env: CloudflareEnv) {}
  async limit(key: string): Promise<boolean> {
    const binding = limitClass(key) === 'write' ? this.env.RL_WRITE : this.env.RL_READ;
    if (!binding) return this.fallback.limit(key);
    return (await binding.limit({ key })).success;
  }
}

let cached: { env: CloudflareEnv; services: Services } | null = null;

function services(env: CloudflareEnv): Services {
  if (cached?.env === env) return cached.services;
  const config = loadConfig(env as unknown as RawEnv);
  const s = buildServices(config, {
    runtime: 'cloudflare',
    repo: new D1Repository(env.DB),
    rateLimiter: new CloudflareRateLimiter(env),
    // Cloudflare has no ambient AWS identity: a dedicated IAM user whose only
    // permission is sts:AssumeRole is configured via Worker secrets.
    awsIdentity: () =>
      env.REGISTRY_AWS_ACCESS_KEY_ID && env.REGISTRY_AWS_SECRET_ACCESS_KEY
        ? { accessKeyId: env.REGISTRY_AWS_ACCESS_KEY_ID, secretAccessKey: env.REGISTRY_AWS_SECRET_ACCESS_KEY }
        : null,
  });
  cached = { env, services: s };
  return s;
}

const app = createApp((c) => services(c.env as CloudflareEnv));

export default {
  fetch: app.fetch,
  async scheduled(_event: ScheduledController, env: CloudflareEnv): Promise<void> {
    const s = services(env);
    await new D1Repository(env.DB).purgeExpired(Date.now(), s.config.auditRetentionDays);
    log({ level: 'info', msg: 'purged expired sessions/cache/audit' });
  },
} satisfies ExportedHandler<CloudflareEnv>;
