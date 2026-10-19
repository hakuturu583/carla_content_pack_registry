// Creates the local SeaweedFS buckets and sample content packs used by the
// mock-login scenarios. Local development only.
//   alice-packs   private-capable bucket owned by alice (alice's own keys)
//   bob-packs     bucket owned by bob (bob's own keys)
//   exposed-packs bucket with anonymous read enabled (to exercise exposure detection)
import { AwsClient } from 'aws4fetch';

const endpoint = (process.env.LOCAL_S3_ENDPOINT ?? 'http://127.0.0.1:8333').replace(/\/+$/, '');
const admin = new AwsClient({
  accessKeyId: 'LOCALSEEDADMIN0001',
  secretAccessKey: 'local-seed-admin-secret-not-for-production',
  service: 's3',
  region: 'us-east-1',
});

const BUCKETS: Record<string, Record<string, string>> = {
  'alice-packs': {
    'maps/Town10HD_Opt.tar.zst': 'fake map pack: Town10HD_Opt\n',
    'maps/readme.md': '# Alice maps\n',
    'vehicles/sedan with space.tar.zst': 'fake vehicle pack\n',
  },
  'bob-packs': { 'props/cones.tar.zst': 'fake prop pack\n' },
  'exposed-packs': { 'leak.txt': 'this bucket is publicly readable\n' },
};

async function ok(res: Response, what: string) {
  if (!res.ok && res.status !== 409) throw new Error(`${what}: HTTP ${res.status} ${await res.text()}`);
}

for (const [bucket, objects] of Object.entries(BUCKETS)) {
  await ok(await admin.fetch(`${endpoint}/${bucket}`, { method: 'PUT' }), `create ${bucket}`);
  for (const [key, body] of Object.entries(objects)) {
    const path = key.split('/').map(encodeURIComponent).join('/');
    await ok(await admin.fetch(`${endpoint}/${bucket}/${path}`, { method: 'PUT', body }), `put ${bucket}/${key}`);
  }
  console.log(`seeded ${bucket} (${Object.keys(objects).length} objects)`);
}
