// Structured JSON logging. Values under sensitive-looking keys are redacted so
// a careless log call cannot leak credentials, tokens or cookies.
const SENSITIVE = /secret|token|password|authorization|cookie|credential|ciphertext|signature/i;

function redact(v: unknown, depth = 0): unknown {
  if (depth > 5 || v === null || typeof v !== 'object') return v;
  if (Array.isArray(v)) return v.map((x) => redact(x, depth + 1));
  const out: Record<string, unknown> = {};
  for (const [k, val] of Object.entries(v)) out[k] = SENSITIVE.test(k) ? '[REDACTED]' : redact(val, depth + 1);
  return out;
}

export function log(event: Record<string, unknown>): void {
  console.log(JSON.stringify({ ts: new Date().toISOString(), ...(redact(event) as object) }));
}
