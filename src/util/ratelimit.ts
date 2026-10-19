// Application-level rate limiting (per GitHub user, per user+bucket).
import type { Clock, RateLimiter } from '../core/ports.ts';

export type LimitClass = 'write' | 'read';

/** Keys end with ":write" / ":verify" for management calls; everything else is "read". */
export function limitClass(key: string): LimitClass {
  return key.endsWith(':write') || key.endsWith(':verify') ? 'write' : 'read';
}

export const DEFAULT_LIMITS: Record<LimitClass, { limit: number; windowSeconds: number }> = {
  write: { limit: 30, windowSeconds: 60 },
  read: { limit: 120, windowSeconds: 60 },
};

/** Fixed-window limiter held in isolate memory. Suitable for local dev and tests only. */
export class MemoryRateLimiter implements RateLimiter {
  private readonly counters = new Map<string, { window: number; count: number }>();
  constructor(
    private readonly clock: Clock,
    private readonly limits = DEFAULT_LIMITS,
  ) {}

  async limit(key: string): Promise<boolean> {
    const { limit, windowSeconds } = this.limits[limitClass(key)];
    const window = Math.floor(this.clock.now() / 1000 / windowSeconds);
    const c = this.counters.get(key);
    if (!c || c.window !== window) {
      if (this.counters.size > 10_000) this.counters.clear();
      this.counters.set(key, { window, count: 1 });
      return true;
    }
    c.count++;
    return c.count <= limit;
  }
}
