import { BatchGetCommand, QueryCommand, type DynamoDBDocumentClient } from '@aws-sdk/lib-dynamodb';
import { describe, expect, it } from 'vitest';
import type { AclEntry, AclRef, Bucket } from '../../src/core/types.ts';
import { MockIdentityProvider } from '../../src/identity/mock.ts';
import { D1Repository } from '../../src/repository/d1.ts';
import { DynamoRepository } from '../../src/repository/dynamodb.ts';
import { MemoryRepository } from '../../src/repository/memory.ts';
import { createSqliteD1 } from '../helpers/d1-sqlite.ts';
import { LOCAL_KEYS } from '../helpers/fake-s3.ts';
import { inProcess } from '../helpers/harness.ts';

/** Mock identity that resolves any "uNNN" login to user id NNN. */
class ManyUsers extends MockIdentityProvider {
  override async resolvePrincipal(token: string, ref: AclRef): Promise<AclEntry> {
    if (ref.type === 'user' && /^u\d+$/.test(ref.login)) return { type: 'user', id: 100000 + Number(ref.login.slice(1)), name: ref.login };
    return super.resolvePrincipal(token, ref);
  }
}

const users = (from: number, n: number) => Array.from({ length: n }, (_, i) => ({ type: 'user', login: `u${from + i}` }));

describe.each([
  ['memory', () => new MemoryRepository()],
  ['d1', () => new D1Repository(createSqliteD1())],
])('ACL update size (%s)', (_name, makeRepo) => {
  it('caps additions + removals per update so every backend can apply it atomically', async () => {
    const { client } = inProcess(makeRepo(), { identity: new ManyUsers() });
    const b = await client.call('POST', '/v1/buckets', {
      as: 'alice',
      body: { name: 'big', storage: { provider: 'local', bucket: 'alice-packs' }, credentials: LOCAL_KEYS.alice },
    });
    const put = (entries: unknown[]) => client.call('PUT', `/v1/buckets/${b.body.id}/acl`, { as: 'alice', body: { entries } });
    expect((await put(users(0, 99))).status).toBe(200); // 99 additions
    expect((await put([...users(0, 99), ...users(99, 1)])).status).toBe(200); // 1 addition
    const swap = await put(users(200, 100)); // 100 additions + 100 removals
    expect(swap.status).toBe(400);
    expect(swap.body.error.message).toMatch(/smaller steps/);
    expect((await client.call('GET', `/v1/buckets/${b.body.id}/acl`, { as: 'alice' })).body.entries).toHaveLength(100);
  });
});

describe('DynamoRepository.listCandidateBuckets', () => {
  it('retries UnprocessedKeys instead of dropping buckets', async () => {
    const bucket = (id: string) => ({ id, ownerId: 2, createdAt: id, visibility: 'private' }) as Bucket;
    let batchCalls = 0;
    const doc = {
      async send(cmd: unknown) {
        if (cmd instanceof QueryCommand) {
          const pk = cmd.input.ExpressionAttributeValues?.[':pk'];
          return { Items: pk === 'PRINCIPAL#user#1' ? [{ PK: 'BUCKET#a' }, { PK: 'BUCKET#b' }] : [] };
        }
        if (cmd instanceof BatchGetCommand) {
          batchCalls++;
          const keys = cmd.input.RequestItems!.t!.Keys as { PK: string }[];
          // First call: return one item, leave the other unprocessed (throttled).
          const [first, ...rest] = keys;
          return {
            Responses: { t: [{ data: bucket(first!.PK.slice('BUCKET#'.length)) }] },
            UnprocessedKeys: rest.length ? { t: { Keys: rest } } : {},
          };
        }
        throw new Error('unexpected command');
      },
    } as unknown as DynamoDBDocumentClient;
    const ids = (await new DynamoRepository(doc, 't').listCandidateBuckets(1, [{ type: 'user', id: 1 }])).map((b) => b.id);
    expect(ids.sort()).toEqual(['a', 'b']);
    expect(batchCalls).toBe(2);
  });
});
