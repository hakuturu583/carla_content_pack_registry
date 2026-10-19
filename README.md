# CARLA Content Pack Registry

CARLA 用 Content Pack を発見・共有・ダウンロードするための Registry サービスです。

- 認証は **GitHub OAuth App**。ユーザー / Organization / Team は GitHub の **数値 ID** で識別します。
- **BYOB (Bring Your Own Bucket)**: 各 GitHub ユーザーが自分の **Cloudflare R2** または **AWS S3** バケットを登録します。Registry 運営者はストレージ料金を負担しません。
- ファイル本体は R2/S3 から **署名付き URL（最大 5 分）で直接ダウンロード**します。Registry API はデータを中継しません。
- 同じ Registry API（同じコード、同じ OpenAPI 契約）を **Cloudflare Workers + D1** と **AWS Lambda + API Gateway + DynamoDB** のどちらにもデプロイできます。
- インフラは Terraform（+ Wrangler）と GitHub Actions で再現可能です。

> **現在の状態**: 実装とローカル検証は完了していますが、**実クラウド（R2 / S3 / Workers / Lambda / 本物の GitHub OAuth App）での検証はまだ行っていません**。
> 各 Phase のテスト結果と残課題は [docs/status.md](docs/status.md) を参照してください。
> 指示書で起点とされた `registry-local-prototype-seaweedfs` はこのリポジトリにもセッションにも存在しなかったため、仕様書から新規に実装しています（[docs/phase0-audit.md](docs/phase0-audit.md)）。

## ドキュメント

| ドキュメント | 内容 |
| --- | --- |
| [docs/status.md](docs/status.md) | Phase ごとの実装状況・テスト結果・残課題、受け入れ条件の状況 |
| [docs/phase0-audit.md](docs/phase0-audit.md) | Phase 0 監査（PoC 不在の記録、依存関係・ライセンス、PoC の既知課題への対応） |
| [docs/architecture.md](docs/architecture.md) | コンポーネント構成、データモデル、認証・ダウンロードの流れ |
| [docs/security.md](docs/security.md) | セキュリティ要件への対応表と残留リスク |
| [docs/runbook.md](docs/runbook.md) | 環境構築、シークレット登録、デプロイ、ロールバック、バックアップ/復旧、障害対応、バケット所有者向け手順 |
| [openapi/openapi.yaml](openapi/openapi.yaml) | API 契約（Cloudflare 版・AWS 版共通） |

## 構成

```
src/
  core/          RegistryCore（登録・ACL・認可・ダウンロード）、ポート定義、入力検証
  identity/      GitHubIdentityProvider（OAuth / Org・Team メンバーシップ）、開発用 Mock
  storage/       StorageProvider（S3 互換: R2 / AWS S3 / ローカル SeaweedFS）
  credentials/   CredentialResolver（AES-GCM 暗号化、STS AssumeRole）
  repository/    RegistryRepository（D1 / DynamoDB / テスト用メモリ）
  http/          Hono アプリ（両ランタイム共通の API）、セッション・CSRF、簡易 UI
  runtime/       CloudflareRuntime（Workers + D1）、AWSRuntime（Lambda + DynamoDB）、設定
migrations/      D1 スキーマ
openapi/         OpenAPI 3.1
infra/           Terraform（cloudflare / aws / aws/bootstrap）
deploy/          wrangler 設定（ローカル用・環境別テンプレート）
local/           Docker Compose（SeaweedFS、DynamoDB Local）
.github/         CI とデプロイワークフロー
```

## ローカル開発（Wrangler + D1 Local + SeaweedFS + Mock 認証）

前提: Node.js 22.13 以上、Docker（Compose v2）。

```bash
npm ci
npm run dev:storage                 # SeaweedFS (127.0.0.1:8333) を起動
npm run dev:seed                    # ローカル用バケットとサンプル Content Pack を作成
node scripts/gen-dev-vars.mjs       # deploy/cloudflare/.dev.vars（暗号鍵）を生成
npm run db:migrate:local            # D1 Local にスキーマ適用
npm run dev                         # http://localhost:8787
```

ブラウザで http://localhost:8787 を開き、「Sign in with GitHub」から Mock ユーザーを選択します。

| Mock ユーザー | GitHub ID | 所属 |
| --- | --- | --- |
| alice | 1001 | org `carla-sim`（team `maps`）、org `other-lab` |
| bob | 1002 | org `carla-sim`（team `vehicles`） |
| carol | 1003 | なし |

SeaweedFS にはユーザーごとに別々のアクセスキーを設定しています（共通の開発用資格情報はありません）。
`alice-packs` は alice のキーでのみ、`bob-packs` は bob のキーでのみ読めます。`exposed-packs` は匿名読み取りが有効で、Private 登録時の公開検出を確認するためのものです。

CLI からは Bearer トークンで呼べます（ローカルの Mock モードでは `mock:<login>`）:

```bash
curl -s -H 'authorization: Bearer mock:alice' -H 'content-type: application/json' \
  -d '{"name":"alice-maps","visibility":"private","storage":{"provider":"local","bucket":"alice-packs"},
       "credentials":{"accessKeyId":"LOCALALICEKEY00001","secretAccessKey":"local-alice-secret-not-for-production"}}' \
  http://localhost:8787/v1/buckets
# => {"id":"<uuid>", ... "verifyStatus":"unverified"}
curl -s -X PUT -H 'authorization: Bearer mock:alice' -H 'content-type: application/json' \
  -d '{"entries":[{"type":"team","org":"carla-sim","slug":"maps"}]}' http://localhost:8787/v1/buckets/<uuid>/acl
curl -s -X POST -H 'authorization: Bearer mock:alice' http://localhost:8787/v1/buckets/<uuid>/verify
curl -s -H 'authorization: Bearer mock:alice' http://localhost:8787/v1/buckets/<uuid>/objects
curl -s -H 'authorization: Bearer mock:alice' http://localhost:8787/v1/buckets/<uuid>/objects/maps/readme.md/download
```

本物の GitHub OAuth App をローカルで試す場合は `.dev.vars` に `AUTH_MODE="github"` と `GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET` を設定し、OAuth App の Callback URL を `http://localhost:8787/login/github/callback` にします。

Mock 認証は `ENVIRONMENT=local` 以外では起動時に拒否されます（設定エラーとして全リクエストが 500 になります）。

## テスト

```bash
npm run typecheck && npm run lint
npm test                            # ユニット + API 契約テスト（Memory / D1(SQLite) / Fake S3）
docker compose -f local/docker-compose.yml --profile aws up -d --wait   # SeaweedFS + DynamoDB Local
npm run dev:seed
npm run test:integration            # Worker エントリ + D1、Lambda ハンドラ + DynamoDB Local、実 SeaweedFS
SMOKE_BASE_URL=http://localhost:8787 SMOKE_TOKEN=mock:alice SMOKE_LOCAL_FLOW=1 npm run smoke   # wrangler dev に対して
```

`test/helpers/scenarios.ts` の API 契約シナリオ（Alice/Bob/Carol による Public / Private / User / Org / Team ACL、所有者限定操作、公開検出、署名付きダウンロードなど）を、
インプロセス（Memory, D1）、Cloudflare Worker エントリ、AWS Lambda ハンドラのすべてで同一に実行し、Cloudflare 版と AWS 版が同じ API・認可動作になることを確認しています。

## デプロイ

概要のみ記載します。手順の詳細・前提（承認、費用、資格情報の登録）は [docs/runbook.md](docs/runbook.md) を参照してください。
**このリポジトリの作業ではクラウドリソースの作成・デプロイは一切行っていません。**

- Cloudflare: `infra/cloudflare`（D1 / DNS / WAF / Bot 対策 / 任意で STS 用 IAM ユーザー）を Terraform で適用 → `scripts/render-wrangler.mjs` で wrangler 設定を生成 → D1 マイグレーション → `wrangler deploy` → スモークテスト。
- AWS: `infra/aws/bootstrap`（state バケット、GitHub OIDC デプロイロール）を一度だけ適用 → `infra/aws`（API Gateway REST / Lambda / DynamoDB / WAF / Secrets Manager / CloudWatch / Budget）を Terraform で適用 → スモークテスト。
- GitHub Actions: `.github/workflows/deploy-cloudflare.yml` / `deploy-aws.yml`（`workflow_dispatch`、plan → 承認付き環境で apply → デプロイ → スモークテスト）。
- 各ユーザー所有の R2/S3 バケットは運営側 Terraform の管理対象外です。

## ライセンス

リポジトリのライセンスは未決定です（`package.json` は `UNLICENSED`）。依存関係のライセンスは [docs/phase0-audit.md](docs/phase0-audit.md) を参照してください。
