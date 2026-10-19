// Composition root shared by all runtimes. Platform adapters (repository, rate
// limiter, registry AWS identity) are injected by runtime/cloudflare.ts or runtime/aws.ts.
import { type Clock, type RateLimiter, type RegistryRepository, systemClock } from '../core/ports.ts';
import { RegistryCore } from '../core/registry.ts';
import { DefaultCredentialResolver, type RegistryAwsIdentity } from '../credentials/resolver.ts';
import { AuthService } from '../http/auth.ts';
import { GitHubIdentityProvider, GitHubOAuth } from '../identity/github.ts';
import { MockIdentityProvider } from '../identity/mock.ts';
import { S3CompatibleStorageFactory } from '../storage/s3compat.ts';
import { Keyring } from '../util/crypto.ts';
import { log } from '../util/log.ts';
import type { RegistryConfig } from './config.ts';

export interface PlatformAdapters {
  runtime: 'cloudflare' | 'aws' | 'test';
  repo: RegistryRepository;
  rateLimiter: RateLimiter;
  awsIdentity: RegistryAwsIdentity['credentials'];
  fetchImpl?: typeof fetch;
  clock?: Clock;
}

export interface Services {
  config: RegistryConfig;
  runtime: PlatformAdapters['runtime'];
  core: RegistryCore;
  auth: AuthService;
  repo: RegistryRepository;
}

export function buildServices(config: RegistryConfig, p: PlatformAdapters): Services {
  const clock = p.clock ?? systemClock;
  const keyring = Keyring.parse(config.credentialKeys);
  const identity =
    config.authMode === 'mock' ? new MockIdentityProvider() : new GitHubIdentityProvider('carla-content-pack-registry', p.fetchImpl);
  const oauth = config.github
    ? new GitHubOAuth({ ...config.github, redirectUri: `${config.publicBaseUrl}/login/github/callback` }, p.fetchImpl)
    : null;
  const credentials = new DefaultCredentialResolver(
    keyring,
    { credentials: p.awsIdentity, stsRegion: config.aws.stsRegion },
    clock,
    p.fetchImpl,
  );
  const storage = new S3CompatibleStorageFactory(credentials, config.local, clock, p.fetchImpl);
  const core = new RegistryCore(
    {
      repo: p.repo,
      identity,
      storage,
      credentials,
      rateLimiter: p.rateLimiter,
      clock,
      newId: () => crypto.randomUUID(),
      log,
    },
    {
      presignTtlSeconds: config.presignTtlSeconds,
      membershipCacheSeconds: config.membershipCacheSeconds,
      allowLocalStorage: config.local !== null,
      awsPrincipalArn: config.aws.principalArn,
    },
  );
  const auth = new AuthService({
    repo: p.repo,
    identity,
    oauth,
    keyring,
    clock,
    sessionTtlSeconds: config.sessionTtlSeconds,
    secureCookies: new URL(config.publicBaseUrl).protocol === 'https:',
    publicOrigin: new URL(config.publicBaseUrl).origin,
  });
  return { config, runtime: p.runtime, core, auth, repo: p.repo };
}
