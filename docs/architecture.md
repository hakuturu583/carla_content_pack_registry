# アーキテクチャ

## コンポーネント

指示書の主要コンポーネントとコードの対応です。業務ロジック（`src/core`）は Cloudflare / AWS / GitHub の型や API に依存せず、すべて `src/core/ports.ts` のインターフェース経由で外部に触れます。

| 指示書のコンポーネント | 実装 |
| --- | --- |
| RegistryCore | `src/core/registry.ts`（登録・更新・削除、ACL、検証、一覧、ダウンロード）、`src/core/authz.ts`（純粋関数の認可規則）、`src/core/validation.ts` |
| GitHubIdentityProvider | `src/identity/github.ts`（OAuth Web フロー + PKCE、`/user`、`/user/memberships/orgs`、`/user/teams`、名前→数値 ID 解決）。開発用は `src/identity/mock.ts` |
| RegistryRepository | `src/repository/d1.ts`（D1）、`src/repository/dynamodb.ts`（DynamoDB 単一テーブル）、`src/repository/memory.ts`（テスト用） |
| StorageProvider | `src/storage/s3compat.ts`（R2 / AWS S3 / SeaweedFS 共通。ListObjectsV2、HeadObject、署名付き GET、検証） |
| CredentialResolver | `src/credentials/resolver.ts`（R2: 暗号化した静的キーの復号、S3: STS AssumeRole + ExternalId + セッションポリシー） |
| CloudflareRuntime | `src/runtime/cloudflare.ts`（Workers の `fetch` / `scheduled`、D1、Workers Rate Limiting バインディング） |
| AWSRuntime | `src/runtime/aws.ts`（API Gateway REST → Lambda、DynamoDB、Secrets Manager、DynamoDB によるレート制限） |
| CLI | `cli/`（Rust コア `carla_pack_registry`: API クライアント・デバイスフロー・トークン保存・ダウンロード・Content Pack の検証付きインストール。Python フロントエンド `carla_pack_registry.cli`: 引数解析と表示、`carla_pack_registry.deploy_pack`: CARLA サーバへのマウント） |
| HTTP API（共通） | `src/http/app.ts`（Hono）、`src/http/auth.ts`（セッション、CSRF、OAuth state） |

```
            Browser / CARLA CLI
                    │
      Cloudflare WAF │ AWS WAF (API Gateway REST ステージに関連付け)
                    │
   ┌────────────────┴─────────────────┐
   │ runtime/cloudflare.ts │ runtime/aws.ts │  ← 設定読込・アダプタ生成のみ
   └────────────────┬─────────────────┘
                    │ runtime/services.ts（共通の組み立て）
              http/app.ts（同一 API）
                    │
              core/RegistryCore ──── IdentityProvider ── GitHub API
                    │                 RateLimiter
                    ├── RegistryRepository ── D1 | DynamoDB
                    ├── CredentialResolver ── Keyring(AES-GCM) | STS AssumeRole
                    └── StorageProvider ───── R2 | S3 | SeaweedFS (S3 API, SigV4)
                                                   ↑
                    クライアントは署名付き URL で直接ダウンロード
```

## 4 つの組み合わせ

Registry の配置先とユーザーのストレージは独立しています。どちらのランタイムでも同じ `S3CompatibleStorageFactory` と `DefaultCredentialResolver` を使い、違いは「Registry 自身の AWS プリンシパルの資格情報をどこから得るか」だけです。

| Registry | ストレージ | 資格情報の流れ |
| --- | --- | --- |
| Workers | R2 | D1 上の暗号化キーを復号 → `https://<account>.r2.cloudflarestorage.com/<bucket>` に SigV4 |
| Workers | S3 | Worker シークレットの IAM ユーザーキーで `sts:AssumeRole`（ExternalId 付き）→ 一時資格情報で S3 |
| Lambda | R2 | DynamoDB 上の暗号化キーを復号（鍵は Secrets Manager）→ R2 |
| Lambda | S3 | Lambda 実行ロールで `sts:AssumeRole` → S3 |

## データモデル

- **User**: GitHub 数値 ID、login（表示用）。
- **Bucket**: UUID、`ownerId`（GitHub ユーザー ID、変更不可）、名前、説明、`visibility`（public/private）、ストレージ設定（秘密情報なし）、暗号化済み資格情報（R2/local のみ）、検証状態。
- **ACL エントリ**: `(type, id, name)`。`type` は `user` / `org` / `team`、`id` は GitHub 数値 ID、`name` は書き込み時点の表示名。
- **Session**: Cookie 値の SHA-256、ユーザー ID、暗号化した GitHub トークン、CSRF トークン、有効期限（既定 8 時間）。
- **Membership cache**: ユーザーごとの org/team ID 一覧（既定 5 分）。
- **Audit log**: 時刻、操作者、操作、バケット、結果（ok/allow/deny/error）、詳細。

D1 は `migrations/0001_init.sql`、DynamoDB は `src/repository/dynamodb.ts` 冒頭のキー設計（GSI1: 所有者・ACL 主体、GSI2: Public バケット）を参照してください。

## 認可

`src/core/authz.ts`:

1. 所有者 → 常に許可（閲覧・管理）
2. Public バケット → ログイン済みの全 GitHub ユーザーが閲覧可
3. Private バケット → ACL のいずれかに一致すれば閲覧可
   - `user`: 閲覧者の GitHub ID と一致
   - `org`: 閲覧者の **active** な Organization メンバーシップに含まれる
   - `team`: 閲覧者の Team メンバーシップに含まれる
4. 管理操作（設定更新、ACL、資格情報、検証、登録解除）は所有者のみ
5. GitHub がメンバーシップを確認できない場合（API 失敗、`read:org` 不足、Org が OAuth App を未承認、レート制限）は **拒否**
6. 閲覧できない Private バケットは 404 を返し、存在を開示しない

ACL の書き込み時は、所有者のトークンで login / slug を数値 ID に解決して保存します（名前の変更・再利用の影響を受けません）。

## 認証の流れ

- **ブラウザ**: `/login/github` → GitHub（`state` と PKCE `code_challenge`、スコープ `read:org`）→ `/login/github/callback` で state を Cookie と照合し、`code_verifier` 付きでトークン交換 → セッション Cookie（`__Host-` 接頭辞、HttpOnly、Secure、SameSite=Lax）。
- **CLI（`carla-pack`）**: `GET /cli/config` で OAuth App の client ID を取得し、GitHub のデバイスフロー（スコープ `read:org`）でトークンを取得します。以降は `Authorization: Bearer <GitHub OAuth トークン>` で API を呼びます（Bearer リクエストは CSRF の対象外）。トークンは CLI の設定ディレクトリ（パーミッション 600）に保存します。ダウンロードは署名付き URL から直接行い、Registry のトークンはストレージに送りません。
- **CSRF**: Cookie セッションでの POST/PUT/PATCH/DELETE は `X-CSRF-Token`（`/v1/me` で取得）必須。`Origin` ヘッダーがある場合は自オリジンのみ許可。

## ダウンロードの流れ

1. クライアント → `GET /v1/buckets/{id}/objects/{key}/download`
2. Registry が GitHub ユーザーを認証（セッションまたは Bearer）
3. ACL を判定（必要なら GitHub の Org/Team メンバーシップを確認、5 分キャッシュ）
4. バケットが検証済みであることを確認し、HeadObject で存在確認
5. 有効期限 300 秒以下の署名付き GET URL を返す（`?redirect=1` なら 302）
6. クライアントは R2/S3 から直接ダウンロード

## バケット登録と検証

1. `POST /v1/buckets` — R2 はアカウント ID・バケット名・バケット限定トークン、S3 はリージョン・バケット名・ロール ARN。S3 の場合 Registry が ExternalId を生成し、所有者にだけ `awsTrust`（信頼すべきプリンシパル ARN と ExternalId）を返します。
2. `POST /v1/buckets/{id}/verify` — 一覧・HeadObject ができること、Private の場合は匿名の一覧・読み取りが拒否されることを確認。ListBuckets が成功する（＝バケット限定でない）資格情報には警告を出します。
3. 検証済みになるまで、オブジェクト一覧とダウンロードは 409 `not_verified` です。Public → Private への変更や資格情報の更新で未検証に戻ります。
