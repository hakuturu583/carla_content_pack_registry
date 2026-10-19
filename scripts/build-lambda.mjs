// Bundles the AWS Lambda handler into dist/lambda/index.mjs (Node.js 22, ESM).
// The AWS SDK is bundled (pinned) rather than taken from the runtime.
import { build } from 'esbuild';

await build({
  entryPoints: ['src/runtime/aws.ts'],
  outfile: 'dist/lambda/index.mjs',
  bundle: true,
  platform: 'node',
  target: 'node22',
  format: 'esm',
  minify: true,
  sourcemap: true,
  legalComments: 'linked',
  banner: { js: "import { createRequire } from 'module'; const require = createRequire(import.meta.url);" },
});
console.log('built dist/lambda/index.mjs');
