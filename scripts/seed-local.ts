// Creates the local SeaweedFS buckets and sample content packs used by the
// mock-login scenarios (see scripts/local-fixtures.ts). Local development only.
import { AwsClient } from 'aws4fetch';
import { encodeKey } from '../src/util/s3key.ts';
import { LOCAL_BUCKETS } from './local-fixtures.ts';

const endpoint = (process.env.LOCAL_S3_ENDPOINT ?? 'http://127.0.0.1:8333').replace(/\/+$/, '');
const admin = new AwsClient({
  accessKeyId: 'LOCALSEEDADMIN0001',
  secretAccessKey: 'local-seed-admin-secret-not-for-production',
  service: 's3',
  region: 'us-east-1',
});

async function ok(res: Response, what: string) {
  if (!res.ok && res.status !== 409) throw new Error(`${what}: HTTP ${res.status} ${await res.text()}`);
}

for (const [bucket, { objects }] of Object.entries(LOCAL_BUCKETS)) {
  await ok(await admin.fetch(`${endpoint}/${bucket}`, { method: 'PUT' }), `create ${bucket}`);
  for (const [key, body] of Object.entries(objects)) {
    await ok(await admin.fetch(`${endpoint}/${bucket}/${encodeKey(key)}`, { method: 'PUT', body }), `put ${bucket}/${key}`);
  }
  console.log(`seeded ${bucket} (${Object.keys(objects).length} objects)`);
}
