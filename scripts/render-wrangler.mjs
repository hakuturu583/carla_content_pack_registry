// Renders deploy/cloudflare/wrangler.template.toml for one environment.
//   ENV=staging HOSTNAME=... ZONE_NAME=... D1_DATABASE_ID=... D1_DATABASE_NAME=... \
//   GITHUB_CLIENT_ID=... node scripts/render-wrangler.mjs
// Output: deploy/cloudflare/wrangler.<env>.generated.toml (gitignored)
import { readFileSync, writeFileSync } from 'node:fs';

const env = process.env.ENV;
if (env !== 'staging' && env !== 'production') throw new Error('ENV must be staging or production');
const defaults = {
  REGISTRY_AWS_PRINCIPAL_ARN: '',
  AWS_STS_REGION: 'us-east-1',
  RL_WRITE_NAMESPACE: env === 'staging' ? '2001' : '3001',
  RL_READ_NAMESPACE: env === 'staging' ? '2002' : '3002',
};
const template = readFileSync(new URL('../deploy/cloudflare/wrangler.template.toml', import.meta.url), 'utf8');
const out = template.replace(/\$\{([A-Z0-9_]+)\}/g, (_, k) => {
  const v = process.env[k] ?? defaults[k];
  if (v === undefined) throw new Error(`missing ${k}`);
  if (/["\\\n]/.test(v)) throw new Error(`invalid characters in ${k}`);
  return v;
});
const path = new URL(`../deploy/cloudflare/wrangler.${env}.generated.toml`, import.meta.url);
writeFileSync(path, out);
console.log(`wrote ${path.pathname}`);
