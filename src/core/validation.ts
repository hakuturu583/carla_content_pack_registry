// Input validation for user-supplied bucket settings. Every value that ends up in a
// hostname or URL is checked here, so there is no user-controlled endpoint (SSRF guard).
import { badRequest } from './errors.ts';
import type { StorageProviderKind, Visibility } from './types.ts';

export const LIMITS = {
  nameMax: 64,
  descriptionMax: 1000,
  aclMaxEntries: 100,
  objectKeyMax: 1024,
  listLimitMax: 1000,
} as const;

const NAME_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const R2_ACCOUNT_RE = /^[0-9a-f]{32}$/;
const R2_BUCKET_RE = /^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$/;
// S3 bucket names: dots are rejected so virtual-hosted TLS always works.
const S3_BUCKET_RE = /^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$/;
// Commercial + GovCloud partitions (aws). China / ISO partitions are not supported.
const AWS_REGION_RE = /^(us|eu|ap|sa|ca|me|af|il|mx)(-gov)?-(north|south|east|west|central|northeast|northwest|southeast|southwest)-[1-9]$/;
const ROLE_ARN_RE = /^arn:aws:iam::(\d{12}):role\/[A-Za-z0-9+=,.@_/-]{1,512}$/;
const ACCESS_KEY_RE = /^[A-Za-z0-9]{16,128}$/;
const SECRET_KEY_RE = /^[A-Za-z0-9/+=_-]{16,128}$/;
const LOGIN_RE = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})$/;
const TEAM_SLUG_RE = /^[a-z0-9][a-z0-9_-]{0,99}$/;

type Json = Record<string, unknown>;

export function asObject(v: unknown, what = 'body'): Json {
  if (typeof v !== 'object' || v === null || Array.isArray(v)) throw badRequest(`${what} must be a JSON object`);
  return v as Json;
}

function str(o: Json, key: string, required: true): string;
function str(o: Json, key: string, required: false): string | undefined;
function str(o: Json, key: string, required: boolean): string | undefined {
  const v = o[key];
  if (v === undefined || v === null) {
    if (required) throw badRequest(`${key} is required`);
    return undefined;
  }
  if (typeof v !== 'string') throw badRequest(`${key} must be a string`);
  return v;
}

function check(value: string, re: RegExp, field: string): string {
  if (!re.test(value)) throw badRequest(`${field} is invalid`, { field });
  return value;
}

export function validateName(v: string): string {
  return check(v, NAME_RE, 'name');
}

export function validateDescription(v: string): string {
  if (v.length > LIMITS.descriptionMax) throw badRequest('description is too long');
  return v;
}

export function validateVisibility(v: string): Visibility {
  if (v !== 'public' && v !== 'private') throw badRequest('visibility must be "public" or "private"');
  return v;
}

export interface StorageInput {
  provider: StorageProviderKind;
  bucket: string;
  accountId?: string;
  region?: string;
  roleArn?: string;
}

export function parseStorageInput(raw: unknown, allowLocal: boolean): StorageInput {
  const o = asObject(raw, 'storage');
  const provider = str(o, 'provider', true);
  const bucket = str(o, 'bucket', true);
  switch (provider) {
    case 'r2':
      return {
        provider,
        bucket: check(bucket, R2_BUCKET_RE, 'storage.bucket'),
        accountId: check(str(o, 'accountId', true), R2_ACCOUNT_RE, 'storage.accountId'),
      };
    case 's3':
      return {
        provider,
        bucket: check(bucket, S3_BUCKET_RE, 'storage.bucket'),
        region: check(str(o, 'region', true), AWS_REGION_RE, 'storage.region'),
        roleArn: check(str(o, 'roleArn', true), ROLE_ARN_RE, 'storage.roleArn'),
      };
    case 'local':
      if (!allowLocal) throw badRequest('storage.provider "local" is only available in local development');
      return { provider, bucket: check(bucket, R2_BUCKET_RE, 'storage.bucket') };
    default:
      throw badRequest('storage.provider must be "r2" or "s3"');
  }
}

export function parseStaticCredentials(raw: unknown): { accessKeyId: string; secretAccessKey: string } {
  const o = asObject(raw, 'credentials');
  return {
    accessKeyId: check(str(o, 'accessKeyId', true), ACCESS_KEY_RE, 'credentials.accessKeyId'),
    secretAccessKey: check(str(o, 'secretAccessKey', true), SECRET_KEY_RE, 'credentials.secretAccessKey'),
  };
}

export type AclRef =
  | { type: 'user'; login: string }
  | { type: 'org'; login: string }
  | { type: 'team'; org: string; slug: string };

export function parseAclInput(raw: unknown): AclRef[] {
  const o = asObject(raw);
  const entries = o.entries;
  if (!Array.isArray(entries)) throw badRequest('entries must be an array');
  if (entries.length > LIMITS.aclMaxEntries) throw badRequest(`at most ${LIMITS.aclMaxEntries} ACL entries`);
  return entries.map((e, i) => {
    const eo = asObject(e, `entries[${i}]`);
    const type = str(eo, 'type', true);
    if (type === 'user' || type === 'org') {
      return { type, login: check(str(eo, 'login', true), LOGIN_RE, `entries[${i}].login`) };
    }
    if (type === 'team') {
      return {
        type,
        org: check(str(eo, 'org', true), LOGIN_RE, `entries[${i}].org`),
        slug: check(str(eo, 'slug', true).toLowerCase(), TEAM_SLUG_RE, `entries[${i}].slug`),
      };
    }
    throw badRequest(`entries[${i}].type must be user, org or team`);
  });
}

export function validateObjectKey(key: string): string {
  if (key.length === 0 || key.length > LIMITS.objectKeyMax) throw badRequest('object key length is invalid');
  // Control characters are never legitimate in content pack keys.
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f]/.test(key)) throw badRequest('object key contains control characters');
  return key;
}

export function validatePrefix(prefix: string | undefined): string | undefined {
  if (prefix === undefined || prefix === '') return undefined;
  return validateObjectKey(prefix);
}

export function awsAccountFromRoleArn(arn: string): string | null {
  return ROLE_ARN_RE.exec(arn)?.[1] ?? null;
}

export { str as readString };
