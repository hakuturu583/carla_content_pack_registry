// Writes deploy/cloudflare/.dev.vars with a fresh random CREDENTIAL_KEYS for local use.
// Existing files are kept unless --force is given.
import { existsSync, writeFileSync } from 'node:fs';
import { randomBytes } from 'node:crypto';

const path = new URL('../deploy/cloudflare/.dev.vars', import.meta.url);
if (existsSync(path) && !process.argv.includes('--force')) {
  console.log('deploy/cloudflare/.dev.vars already exists (use --force to overwrite)');
  process.exit(0);
}
const lines = [
  `CREDENTIAL_KEYS="local1:${randomBytes(32).toString('base64')}"`,
  '# To try the real GitHub OAuth flow locally, set AUTH_MODE="github" here and fill in:',
  '# GITHUB_CLIENT_ID=""',
  '# GITHUB_CLIENT_SECRET=""',
  '',
];
writeFileSync(path, lines.join('\n'), { mode: 0o600 });
console.log('wrote deploy/cloudflare/.dev.vars');
