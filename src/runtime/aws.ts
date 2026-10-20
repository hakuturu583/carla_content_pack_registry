// AWS Lambda entry point: API Gateway REST API -> Lambda -> DynamoDB.
// Secrets (GitHub client secret, credential keys) come from Secrets Manager at cold start.
import { DynamoDBClient } from '@aws-sdk/client-dynamodb';
import { GetSecretValueCommand, SecretsManagerClient } from '@aws-sdk/client-secrets-manager';
import { DynamoDBDocumentClient, UpdateCommand } from '@aws-sdk/lib-dynamodb';
import { handle } from 'hono/aws-lambda';
import type { Clock, LimitClass, RateLimiter } from '../core/ports.ts';
import { systemClock } from '../core/ports.ts';
import { createApp } from '../http/app.ts';
import { DynamoRepository } from '../repository/dynamodb.ts';
import { DEFAULT_LIMITS, currentWindow } from '../util/ratelimit.ts';
import { loadConfig, type RawEnv } from './config.ts';
import { buildServices, type Services } from './services.ts';

/** Fixed-window counter in DynamoDB (atomic ADD), shared by all Lambda instances. */
export class DynamoRateLimiter implements RateLimiter {
  constructor(
    private readonly doc: DynamoDBDocumentClient,
    private readonly table: string,
    private readonly clock: Clock = systemClock,
    private readonly limits = DEFAULT_LIMITS,
  ) {}

  async limit(cls: LimitClass, key: string): Promise<boolean> {
    const { limit, windowSeconds, window } = currentWindow(this.limits, cls, this.clock);
    const r = await this.doc.send(
      new UpdateCommand({
        TableName: this.table,
        Key: { PK: `RL#${cls}#${key}`, SK: String(window) },
        UpdateExpression: 'ADD #n :one SET #ttl = :ttl',
        ExpressionAttributeNames: { '#n': 'n', '#ttl': 'ttl' },
        ExpressionAttributeValues: { ':one': 1, ':ttl': (window + 2) * windowSeconds },
        ReturnValues: 'UPDATED_NEW',
      }),
    );
    return Number(r.Attributes?.n ?? 0) <= limit;
  }
}

export interface AwsBootstrapOptions {
  env?: RawEnv;
  dynamo?: DynamoDBClient;
  secrets?: SecretsManagerClient;
}

export async function bootstrapAws(opts: AwsBootstrapOptions = {}): Promise<Services> {
  const env: RawEnv = { ...(opts.env ?? process.env) };
  if (env.SECRETS_ARN) {
    const sm = opts.secrets ?? new SecretsManagerClient({});
    const r = await sm.send(new GetSecretValueCommand({ SecretId: env.SECRETS_ARN }));
    const secret = JSON.parse(r.SecretString ?? '{}') as Record<string, string>;
    // Only these keys may come from the secret.
    for (const k of ['GITHUB_CLIENT_SECRET', 'CREDENTIAL_KEYS']) if (secret[k]) env[k] = secret[k];
  }
  const table = env.TABLE_NAME;
  if (!table) throw new Error('TABLE_NAME is required');
  const config = loadConfig(env);
  const doc = DynamoDBDocumentClient.from(opts.dynamo ?? new DynamoDBClient({}), {
    marshallOptions: { removeUndefinedValues: true },
  });
  return buildServices(config, {
    runtime: 'aws',
    repo: new DynamoRepository(doc, table, config.auditRetentionDays),
    rateLimiter: new DynamoRateLimiter(doc, table),
    // The Lambda execution role is the registry's AWS principal for sts:AssumeRole.
    // Read at call time: the runtime may refresh these variables.
    awsIdentity: () => {
      const live = opts.env ?? process.env;
      return live.AWS_ACCESS_KEY_ID && live.AWS_SECRET_ACCESS_KEY
        ? { accessKeyId: live.AWS_ACCESS_KEY_ID, secretAccessKey: live.AWS_SECRET_ACCESS_KEY, sessionToken: live.AWS_SESSION_TOKEN }
        : null;
    },
  });
}

let servicesPromise: Promise<Services> | null = null;
const app = createApp(() => {
  servicesPromise ??= bootstrapAws().catch((e) => {
    servicesPromise = null; // retry on the next invocation
    throw e;
  });
  return servicesPromise;
});

export const handler = handle(app);
