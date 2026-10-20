// Cloudflare Workers entry point: Workers + D1 (+ Workers Rate Limiting bindings).
import { createApp } from '../http/app.ts';
import { D1Repository } from '../repository/d1.ts';
import { systemClock, type LimitClass, type RateLimiter } from '../core/ports.ts';
import { MemoryRateLimiter } from '../util/ratelimit.ts';
import { log } from '../util/log.ts';
import { loadConfig, type RawEnv } from './config.ts';
import { buildServices, type Services } from './services.ts';

interface RateLimitBinding {
  limit(opts: { key: string }): Promise<{ success: boolean }>;
}

/** Bindings, plus the string vars/secrets read by loadConfig (see deploy/cloudflare/wrangler.*.toml). */
export interface CloudflareEnv {
  DB: D1Database;
  RL_WRITE?: RateLimitBinding;
  RL_READ?: RateLimitBinding;
  [name: string]: unknown;
}

class CloudflareRateLimiter implements RateLimiter {
  private readonly fallback = new MemoryRateLimiter(systemClock);
  constructor(private readonly env: CloudflareEnv) {}
  async limit(cls: LimitClass, key: string): Promise<boolean> {
    const binding = cls === 'write' ? this.env.RL_WRITE : this.env.RL_READ;
    if (!binding) return this.fallback.limit(cls, key);
    return (await binding.limit({ key })).success;
  }
}

let cached: { env: CloudflareEnv; services: Services } | null = null;

function services(env: CloudflareEnv): Services {
  if (cached?.env === env) return cached.services;
  const config = loadConfig(env as RawEnv);
  const s = buildServices(config, {
    runtime: 'cloudflare',
    repo: new D1Repository(env.DB),
    rateLimiter: new CloudflareRateLimiter(env),
    // Cloudflare has no ambient AWS identity: a dedicated IAM user whose only
    // permission is sts:AssumeRole is configured via Worker secrets.
    awsIdentity: () =>
      typeof env.REGISTRY_AWS_ACCESS_KEY_ID === 'string' && typeof env.REGISTRY_AWS_SECRET_ACCESS_KEY === 'string'
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
