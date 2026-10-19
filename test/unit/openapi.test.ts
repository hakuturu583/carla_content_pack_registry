// Keeps openapi/openapi.yaml and the Hono routes in sync (paths + methods).
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { createApp } from '../../src/http/app.ts';

function specOperations(): Set<string> {
  const yaml = readFileSync('openapi/openapi.yaml', 'utf8');
  const paths = yaml.slice(yaml.indexOf('\npaths:\n'));
  const ops = new Set<string>();
  let current: string | null = null;
  for (const line of paths.split('\n')) {
    const p = /^ {2}(\/\S*):\s*$/.exec(line);
    if (p) current = p[1]!;
    const m = /^ {4}(get|post|put|patch|delete):\s*$/.exec(line);
    if (m && current) ops.add(`${m[1]!.toUpperCase()} ${current}`);
  }
  return ops;
}

function appOperations(): Set<string> {
  const app = createApp(() => {
    throw new Error('unused');
  });
  const ops = new Set<string>();
  for (const r of app.routes) {
    if (r.method === 'ALL' || ['/', '/app.js', '/login/mock'].includes(r.path)) continue;
    const path = r.path.replace(/:(\w+)/g, '{$1}').replace('/objects/*', '/objects/{key}/download');
    ops.add(`${r.method} ${path}`);
  }
  return ops;
}

describe('OpenAPI contract', () => {
  it('documents exactly the implemented operations', () => {
    expect([...appOperations()].sort()).toEqual([...specOperations()].sort());
  });

  it('covers every required endpoint from the specification', () => {
    const required = [
      'GET /health',
      'GET /login/github',
      'GET /login/github/callback',
      'POST /logout',
      'GET /v1/me',
      'GET /v1/buckets',
      'POST /v1/buckets',
      'GET /v1/buckets/{id}',
      'PATCH /v1/buckets/{id}',
      'DELETE /v1/buckets/{id}',
      'GET /v1/buckets/{id}/acl',
      'PUT /v1/buckets/{id}/acl',
      'GET /v1/buckets/{id}/objects',
      'GET /v1/buckets/{id}/objects/{key}/download',
      'POST /v1/buckets/{id}/verify',
      'PUT /v1/buckets/{id}/credentials',
    ];
    const ops = specOperations();
    for (const r of required) expect(ops.has(r), r).toBe(true);
  });
});
