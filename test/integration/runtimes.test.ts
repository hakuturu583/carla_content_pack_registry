// Runs the shared API contract against real SeaweedFS through each runtime entry point:
//   1. Cloudflare Worker module (src/runtime/cloudflare.ts) with D1 (SQLite shim)
//   2. AWS Lambda handler (src/runtime/aws.ts) with API Gateway REST events + DynamoDB Local
// Prerequisites: `npm run dev:storage && npm run dev:seed`, and DynamoDB Local
// (`docker compose -f local/docker-compose.yml --profile aws up -d`).
import type { APIGatewayProxyEvent, APIGatewayProxyResult, Context } from 'aws-lambda';
import { beforeAll } from 'vitest';
import { createSqliteD1 } from '../helpers/d1-sqlite.ts';
import { DYNAMO_ENDPOINT, createTable, localDynamo } from '../helpers/dynamo.ts';
import { TEST_KEYS } from '../helpers/harness.ts';
import { type ApiClient, defineApiScenarios } from '../helpers/scenarios.ts';

const S3 = process.env.LOCAL_S3_ENDPOINT ?? 'http://127.0.0.1:8333';
const BASE = 'http://localhost:8787';

const commonEnv = {
  ENVIRONMENT: 'local',
  AUTH_MODE: 'mock',
  PUBLIC_BASE_URL: BASE,
  CREDENTIAL_KEYS: TEST_KEYS,
  LOCAL_S3_ENDPOINT: S3,
};

async function fetchUrl(url: string) {
  const r = await fetch(url);
  return { status: r.status, text: await r.text() };
}

function headersFor(opts: { as?: string; body?: unknown; headers?: Record<string, string> }) {
  const headers: Record<string, string> = { ...(opts.headers ?? {}) };
  if (opts.as) headers.authorization = `Bearer mock:${opts.as}`;
  if (opts.body !== undefined) headers['content-type'] = 'application/json';
  return headers;
}

async function parse(res: Response) {
  const text = await res.text();
  let body: unknown = text;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    /* not JSON */
  }
  return { status: res.status, body, headers: res.headers };
}

beforeAll(async () => {
  const r = await fetch(`${S3}/`).catch(() => null);
  if (!r) throw new Error(`SeaweedFS is not reachable at ${S3}; run npm run dev:storage && npm run dev:seed`);
});

defineApiScenarios('Cloudflare Worker entry + D1 + SeaweedFS', async () => {
  const worker = (await import('../../src/runtime/cloudflare.ts')).default;
  const env = { ...commonEnv, DB: createSqliteD1() };
  const ctx = { waitUntil() {}, passThroughOnException() {}, props: {} } as unknown as ExecutionContext;
  const client: ApiClient = {
    async call(method, path, opts = {}) {
      const req = new Request(`${BASE}${path}`, {
        method,
        headers: headersFor(opts),
        body: opts.body === undefined ? undefined : JSON.stringify(opts.body),
        redirect: 'manual',
      });
      return parse(await worker.fetch!(req as never, env as never, ctx));
    },
    fetchUrl,
  };
  return client;
});

defineApiScenarios('AWS Lambda handler (API Gateway REST) + DynamoDB Local + SeaweedFS', async () => {
  const table = `ccpr-test-${Date.now()}`;
  await createTable(localDynamo(), table);
  Object.assign(process.env, commonEnv, {
    TABLE_NAME: table,
    AWS_REGION: 'us-east-1',
    AWS_ACCESS_KEY_ID: 'local',
    AWS_SECRET_ACCESS_KEY: 'local',
    AWS_ENDPOINT_URL_DYNAMODB: DYNAMO_ENDPOINT,
  });
  const { handler } = await import('../../src/runtime/aws.ts');
  const client: ApiClient = {
    async call(method, path, opts = {}) {
      const u = new URL(`${BASE}${path}`);
      const headers = headersFor(opts);
      headers.host = u.host;
      const query = Object.fromEntries(u.searchParams);
      const event = {
        httpMethod: method,
        path: u.pathname,
        resource: '/{proxy+}',
        headers,
        multiValueHeaders: Object.fromEntries(Object.entries(headers).map(([k, v]) => [k, [v]])),
        queryStringParameters: Object.keys(query).length ? query : null,
        multiValueQueryStringParameters: Object.keys(query).length
          ? Object.fromEntries(Object.entries(query).map(([k, v]) => [k, [v]]))
          : null,
        pathParameters: { proxy: u.pathname.slice(1) },
        stageVariables: null,
        body: opts.body === undefined ? null : JSON.stringify(opts.body),
        isBase64Encoded: false,
        requestContext: {
          httpMethod: method,
          path: `/test${u.pathname}`,
          stage: 'test',
          requestId: crypto.randomUUID(),
          identity: { sourceIp: '127.0.0.1' },
        },
      } as unknown as APIGatewayProxyEvent;
      const r = (await handler(event, {} as Context)) as APIGatewayProxyResult & { multiValueHeaders?: Record<string, string[]> };
      const h = new Headers();
      for (const [k, v] of Object.entries(r.headers ?? {})) h.set(k, String(v));
      for (const [k, vs] of Object.entries(r.multiValueHeaders ?? {})) for (const v of vs) h.append(k, String(v));
      const body = r.isBase64Encoded ? Buffer.from(r.body, 'base64').toString() : r.body;
      return parse(new Response(r.statusCode === 204 || r.statusCode === 302 ? null : body, { status: r.statusCode, headers: h }));
    },
    fetchUrl,
  };
  return client;
});
