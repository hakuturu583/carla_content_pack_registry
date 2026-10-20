// Application-level rate limiting (per GitHub user, per user+bucket).
import type { Clock, LimitClass, RateLimiter } from '../core/ports.ts';

export type Limits = Record<LimitClass, { limit: number; windowSeconds: number }>;

export const DEFAULT_LIMITS: Limits = {
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

  async limit(cls: LimitClass, key: string): Promise<boolean> {
    const { limit, window } = currentWindow(this.limits, cls, this.clock);
    key = `${cls}:${key}`;
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

/** Fixed-window bookkeeping shared by the limiter implementations. */
export function currentWindow(limits: Limits, cls: LimitClass, clock: Clock) {
  const { limit, windowSeconds } = limits[cls];
  return { limit, windowSeconds, window: Math.floor(clock.now() / 1000 / windowSeconds) };
}
