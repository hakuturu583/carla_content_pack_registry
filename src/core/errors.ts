export type ErrorCode =
  | 'bad_request'
  | 'unauthenticated'
  | 'forbidden'
  | 'not_found'
  | 'conflict'
  | 'rate_limited'
  | 'csrf_failed'
  | 'membership_unverifiable'
  | 'storage_error'
  | 'not_verified'
  | 'misconfigured'
  | 'upstream_unavailable'
  | 'internal';

const STATUS: Record<ErrorCode, number> = {
  bad_request: 400,
  unauthenticated: 401,
  forbidden: 403,
  not_found: 404,
  conflict: 409,
  rate_limited: 429,
  csrf_failed: 403,
  membership_unverifiable: 403,
  storage_error: 502,
  not_verified: 409,
  misconfigured: 500,
  upstream_unavailable: 503,
  internal: 500,
};

/** Error whose message is safe to return to clients. Never put secrets in `message` or `details`. */
export class RegistryError extends Error {
  readonly status: number;
  constructor(
    readonly code: ErrorCode,
    message: string,
    readonly details?: Record<string, unknown>,
  ) {
    super(message);
    this.name = 'RegistryError';
    this.status = STATUS[code];
  }
}

export const badRequest = (msg: string, details?: Record<string, unknown>) =>
  new RegistryError('bad_request', msg, details);
export const notFound = (msg = 'Not found') => new RegistryError('not_found', msg);
export const forbidden = (msg = 'Forbidden') => new RegistryError('forbidden', msg);
export const unauthenticated = (msg = 'Authentication required') =>
  new RegistryError('unauthenticated', msg);
