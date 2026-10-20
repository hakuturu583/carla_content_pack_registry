// RegistryRepository on DynamoDB (single-table design).
//
//   PK                      SK                     GSI1PK / GSI1SK                     GSI2PK / GSI2SK
//   USER#<id>               PROFILE
//   USER#<id>               MEMBERSHIPS (ttl)
//   BUCKET#<id>             META                   OWNER#<owner> / <created>#<id>      PUBLIC / <created>#<id> (sparse)
//   BUCKET#<id>             ACL#<type>#<pid>       PRINCIPAL#<type>#<pid> / BUCKET#<id>
//   SESSION#<hash>          SESSION (ttl)
//   AUDIT#<yyyy-mm-dd>      <ts>#<rand> (ttl)
import {
  BatchGetCommand,
  BatchWriteCommand,
  DeleteCommand,
  DynamoDBDocumentClient,
  GetCommand,
  PutCommand,
  QueryCommand,
  TransactWriteCommand,
  type QueryCommandInput,
} from '@aws-sdk/lib-dynamodb';
import type { PrincipalRef, RegistryRepository } from '../core/ports.ts';
import type { AclEntry, AuditEvent, Bucket, GitHubId, Memberships, Session, User } from '../core/types.ts';

type Item = Record<string, unknown>;

const bucketPk = (id: string) => `BUCKET#${id}`;

function bucketItem(b: Bucket): Item {
  const item: Item = {
    PK: bucketPk(b.id),
    SK: 'META',
    GSI1PK: `OWNER#${b.ownerId}`,
    GSI1SK: `${b.createdAt}#${b.id}`,
    type: 'bucket',
    data: b,
  };
  if (b.visibility === 'public') {
    item.GSI2PK = 'PUBLIC';
    item.GSI2SK = `${b.createdAt}#${b.id}`;
  }
  return item;
}

export class DynamoRepository implements RegistryRepository {
  constructor(
    private readonly doc: DynamoDBDocumentClient,
    private readonly table: string,
    private readonly auditRetentionDays = 400,
  ) {}

  private async queryAll(input: Omit<QueryCommandInput, 'TableName'>, max = 1000): Promise<Item[]> {
    const out: Item[] = [];
    let ExclusiveStartKey: Record<string, unknown> | undefined;
    do {
      const r = await this.doc.send(new QueryCommand({ TableName: this.table, ...input, ExclusiveStartKey }));
      out.push(...((r.Items as Item[]) ?? []));
      ExclusiveStartKey = r.LastEvaluatedKey;
    } while (ExclusiveStartKey && out.length < max);
    return out;
  }

  async upsertUser(u: Omit<User, 'createdAt' | 'updatedAt'>): Promise<User> {
    const now = new Date().toISOString();
    const prev = await this.getUser(u.id);
    const user: User = { ...u, createdAt: prev?.createdAt ?? now, updatedAt: now };
    await this.doc.send(new PutCommand({ TableName: this.table, Item: { PK: `USER#${u.id}`, SK: 'PROFILE', type: 'user', data: user } }));
    return user;
  }

  async getUser(id: GitHubId): Promise<User | null> {
    const r = await this.doc.send(new GetCommand({ TableName: this.table, Key: { PK: `USER#${id}`, SK: 'PROFILE' } }));
    return (r.Item?.data as User | undefined) ?? null;
  }

  async createBucket(b: Bucket): Promise<void> {
    await this.doc.send(
      new PutCommand({ TableName: this.table, Item: bucketItem(b), ConditionExpression: 'attribute_not_exists(PK)' }),
    );
  }

  async getBucket(id: string): Promise<Bucket | null> {
    const r = await this.doc.send(new GetCommand({ TableName: this.table, Key: { PK: bucketPk(id), SK: 'META' }, ConsistentRead: true }));
    return (r.Item?.data as Bucket | undefined) ?? null;
  }

  async updateBucket(b: Bucket): Promise<void> {
    // Owner is immutable (no ownership transfer).
    await this.doc.send(
      new PutCommand({
        TableName: this.table,
        Item: bucketItem(b),
        ConditionExpression: 'attribute_exists(PK) AND #d.ownerId = :owner',
        ExpressionAttributeNames: { '#d': 'data' },
        ExpressionAttributeValues: { ':owner': b.ownerId },
      }),
    );
  }

  async deleteBucket(id: string): Promise<void> {
    const items = await this.queryAll({
      KeyConditionExpression: 'PK = :pk',
      ExpressionAttributeValues: { ':pk': bucketPk(id) },
      ProjectionExpression: 'PK, SK',
    });
    for (let i = 0; i < items.length; i += 25) {
      await this.doc.send(
        new BatchWriteCommand({
          RequestItems: { [this.table]: items.slice(i, i + 25).map((it) => ({ DeleteRequest: { Key: { PK: it.PK, SK: it.SK } } })) },
        }),
      );
    }
  }

  async listCandidateBuckets(ownerId: GitHubId, principals: PrincipalRef[]): Promise<Bucket[]> {
    const byIndex = (index: 'GSI1' | 'GSI2', pk: string) =>
      this.queryAll({
        IndexName: index,
        KeyConditionExpression: `${index}PK = :pk`,
        ExpressionAttributeValues: { ':pk': pk },
      });
    // Owned, public and per-principal ACL lookups are independent: run them together.
    const [owned, pub, ...granted] = await Promise.all([
      byIndex('GSI1', `OWNER#${ownerId}`),
      byIndex('GSI2', 'PUBLIC'),
      ...principals.map((p) => byIndex('GSI1', `PRINCIPAL#${p.type}#${p.id}`)),
    ]);
    const found = new Map<string, Bucket>();
    for (const it of [...owned!, ...pub!]) found.set((it.data as Bucket).id, it.data as Bucket);

    const ids = [...new Set(granted.flat().map((r) => String(r.PK).slice('BUCKET#'.length)))].filter((id) => !found.has(id));
    const chunks = Array.from({ length: Math.ceil(ids.length / 100) }, (_, i) => ids.slice(i * 100, i * 100 + 100));
    const pages = await Promise.all(
      chunks.map((chunk) =>
        this.doc.send(new BatchGetCommand({ RequestItems: { [this.table]: { Keys: chunk.map((id) => ({ PK: bucketPk(id), SK: 'META' })) } } })),
      ),
    );
    for (const r of pages) for (const it of (r.Responses?.[this.table] as Item[]) ?? []) found.set((it.data as Bucket).id, it.data as Bucket);
    return [...found.values()];
  }

  async getAcl(bucketId: string): Promise<AclEntry[]> {
    const rows = await this.queryAll({
      KeyConditionExpression: 'PK = :pk AND begins_with(SK, :acl)',
      ExpressionAttributeValues: { ':pk': bucketPk(bucketId), ':acl': 'ACL#' },
      ConsistentRead: true,
    });
    return rows
      .map((r) => r.entry as AclEntry)
      .sort((a, b) => a.type.localeCompare(b.type) || a.id - b.id);
  }

  async replaceAcl(bucketId: string, entries: AclEntry[]): Promise<void> {
    const pk = bucketPk(bucketId);
    const sk = (e: AclEntry) => `ACL#${e.type}#${e.id}`;
    const existing = await this.getAcl(bucketId);
    const nextKeys = new Set(entries.map(sk));
    const ops: NonNullable<ConstructorParameters<typeof TransactWriteCommand>[0]['TransactItems']> = [
      // Guard: the bucket must still exist.
      { ConditionCheck: { TableName: this.table, Key: { PK: pk, SK: 'META' }, ConditionExpression: 'attribute_exists(PK)' } },
      ...existing
        .filter((e) => !nextKeys.has(sk(e)))
        .map((e) => ({ Delete: { TableName: this.table, Key: { PK: pk, SK: sk(e) } } })),
      ...entries.map((e) => ({
        Put: {
          TableName: this.table,
          Item: { PK: pk, SK: sk(e), GSI1PK: `PRINCIPAL#${e.type}#${e.id}`, GSI1SK: pk, type: 'acl', entry: e },
        },
      })),
    ];
    // TransactWriteItems is limited to 100 operations; ACLs are capped at 100 entries,
    // so worst case (100 removals + 100 additions) is split into consecutive transactions.
    for (let i = 0; i < ops.length; i += 100) {
      await this.doc.send(new TransactWriteCommand({ TransactItems: ops.slice(i, i + 100) }));
    }
  }

  async putSession(s: Session): Promise<void> {
    await this.doc.send(
      new PutCommand({
        TableName: this.table,
        Item: { PK: `SESSION#${s.idHash}`, SK: 'SESSION', type: 'session', data: s, ttl: Math.ceil(s.expiresAt / 1000) },
      }),
    );
  }

  async getSession(idHash: string): Promise<Session | null> {
    const r = await this.doc.send(new GetCommand({ TableName: this.table, Key: { PK: `SESSION#${idHash}`, SK: 'SESSION' } }));
    return (r.Item?.data as Session | undefined) ?? null;
  }

  async deleteSession(idHash: string): Promise<void> {
    await this.doc.send(new DeleteCommand({ TableName: this.table, Key: { PK: `SESSION#${idHash}`, SK: 'SESSION' } }));
  }

  async getCachedMemberships(userId: GitHubId): Promise<{ value: Memberships; expiresAt: number } | null> {
    const r = await this.doc.send(new GetCommand({ TableName: this.table, Key: { PK: `USER#${userId}`, SK: 'MEMBERSHIPS' } }));
    return r.Item ? { value: r.Item.data as Memberships, expiresAt: Number(r.Item.expiresAt) } : null;
  }

  async putCachedMemberships(userId: GitHubId, value: Memberships, expiresAt: number): Promise<void> {
    await this.doc.send(
      new PutCommand({
        TableName: this.table,
        Item: { PK: `USER#${userId}`, SK: 'MEMBERSHIPS', data: value, expiresAt, ttl: Math.ceil(expiresAt / 1000) + 3600 },
      }),
    );
  }

  async appendAudit(e: AuditEvent): Promise<void> {
    const rand = crypto.getRandomValues(new Uint32Array(1))[0]!.toString(36);
    await this.doc.send(
      new PutCommand({
        TableName: this.table,
        Item: {
          PK: `AUDIT#${e.ts.slice(0, 10)}`,
          SK: `${e.ts}#${rand}`,
          type: 'audit',
          data: e,
          ttl: Math.ceil(Date.parse(e.ts) / 1000) + this.auditRetentionDays * 86400,
        },
      }),
    );
  }
}
