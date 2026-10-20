// Local development fixtures shared by the SeaweedFS seed script, the smoke test and the
// in-process fake S3 used by unit tests. Must match local/seaweedfs/s3.json.

export const LOCAL_KEYS = {
  alice: { accessKeyId: 'LOCALALICEKEY00001', secretAccessKey: 'local-alice-secret-not-for-production' },
  bob: { accessKeyId: 'LOCALBOBKEY0000001', secretAccessKey: 'local-bob-secret-not-for-production' },
};

/**
 * alice-packs   bucket owned by alice (alice's own keys)
 * bob-packs     bucket owned by bob (bob's own keys)
 * exposed-packs alice's bucket with anonymous read enabled (exercises exposure detection)
 */
export const LOCAL_BUCKETS: Record<string, { owner: keyof typeof LOCAL_KEYS; anonymous: boolean; objects: Record<string, string> }> = {
  'alice-packs': {
    owner: 'alice',
    anonymous: false,
    objects: {
      'maps/Town10HD_Opt.tar.zst': 'fake map pack: Town10HD_Opt\n',
      'maps/readme.md': '# Alice maps\n',
      'vehicles/sedan with space.tar.zst': 'fake vehicle pack\n',
    },
  },
  'bob-packs': { owner: 'bob', anonymous: false, objects: { 'props/cones.tar.zst': 'fake prop pack\n' } },
  'exposed-packs': { owner: 'alice', anonymous: true, objects: { 'leak.txt': 'this bucket is publicly readable\n' } },
};
