# 運用 Runbook

> ここに書かれたクラウド操作（リソース作成、シークレット登録、デプロイ）は**費用が発生し得る、またはアカウントに変更を加える操作**です。実行前に必ず担当者の承認を得てください。
> この Runbook の手順は、まだ実クラウドで通しで実施されていません（[status.md](status.md)）。初回実施時に差分があれば更新してください。

## 0. 環境の分離

| 環境 | Cloudflare | AWS | Terraform state | シークレット |
| --- | --- | --- | --- | --- |
| local | `wrangler dev`（D1 Local） | DynamoDB Local（テストのみ） | なし | `deploy/cloudflare/.dev.vars`（git 管理外） |
| staging | Worker `ccpr-registry-staging`、D1 `ccpr-staging-registry` | `ccpr-staging-*` | `*-ccpr-tfstate-staging` | GitHub Environment `staging`、Worker シークレット（staging）、`ccpr-staging/registry` |
| production | Worker `ccpr-registry-production`、D1 `ccpr-production-registry` | `ccpr-production-*` | `*-ccpr-tfstate-production` | GitHub Environment `production`、Worker シークレット（production）、`ccpr-production/registry` |

- GitHub OAuth App は環境ごとに別に作成します（Callback URL が異なり、client secret を共有しないため）。
- `CREDENTIAL_KEYS` も環境ごとに別の鍵を生成します。
- GitHub Environments: `staging-plan` / `production-plan`（承認なし、plan 用）と `staging` / `production`（**Required reviewers を設定**、apply・デプロイ用）。

## 1. 初回セットアップ

### 1.1 AWS bootstrap（AWS 版、または Terraform state を S3 に置く場合）

```bash
cd infra/aws/bootstrap
terraform init
terraform apply -var state_bucket_prefix=<一意な接頭辞>
# 出力: deploy_role_arns, state_buckets
```

- 出力のロール ARN を GitHub Environment 変数 `AWS_DEPLOY_ROLE_ARN` に、リージョンを `AWS_REGION` に設定します。
- `infra/{cloudflare,aws}/envs/<env>.backend.hcl` の `bucket` を出力のバケット名に置き換えます。
- bootstrap の state はローカルに残ります（秘密情報は含みません）。安全な場所に保管してください。

### 1.2 GitHub OAuth App

1. GitHub → Settings → Developer settings → OAuth Apps → New（運営する Organization 配下に作成することを推奨）。
2. Homepage URL: `https://<hostname>`、Authorization callback URL: `https://<hostname>/login/github/callback`
   （AWS でカスタムドメインなしの場合は `terraform output oauth_callback_url` の値）。
3. CLI 用に **Enable Device Flow** をオンにします。
4. Client ID を GitHub Environment 変数 `GITHUB_OAUTH_CLIENT_ID`（Cloudflare）または `infra/aws/envs/<env>.tfvars` の `github_client_id`（AWS）に設定。
5. Client secret はシークレットとして次節で登録します。

**Organization の OAuth App アクセス制限**: Org が「Third-party application access policy」で制限している場合、その Org のメンバーシップは Org オーナーが OAuth App を承認するまで取得できず、Org/Team ACL によるアクセスは拒否されます。ユーザーにはログイン画面から承認をリクエストしてもらい、Org オーナーが承認します。`/v1/me` の `membershipError` で状況を確認できます。

### 1.3 暗号鍵の生成

```bash
echo "k$(date +%Y%m):$(openssl rand -base64 32)"
```

形式は `<鍵ID>:<base64 32 バイト>`。複数指定時はカンマ区切りで、**先頭の鍵で暗号化**し、全ての鍵で復号できます。

### 1.4 Cloudflare 版

1. Cloudflare API トークン（アカウント: D1 Edit / Workers Scripts Edit、ゾーン: DNS Edit / Zone WAF Edit / Bot Management Edit）を作成し、GitHub Environment シークレット `CLOUDFLARE_API_TOKEN` に登録。変数 `CF_ACCOUNT_ID`、`CF_ZONE_NAME` も設定。
2. `infra/cloudflare/envs/<env>.tfvars` を埋める。
   staging と production が同じゾーンの場合、ゾーン単位の WAF・レート制限・Bot 設定（Cloudflare ではフェーズごとに 1 つ）は production の state だけが持ちます（`manage_zone_rules = true`、`zone_rule_hostnames` に両方のホスト名）。staging は `manage_zone_rules = false` なので、production を一度 apply するまで staging のホスト名には WAF ルールが掛かりません。
3. `Deploy (Cloudflare)` ワークフローを実行（初回は D1 作成のため apply 後にデプロイが走ります）。
4. Worker シークレットを登録（初回と変更時のみ。値は標準入力から入力し、シェル履歴に残さない）:
   ```bash
   ENV=staging ... node scripts/render-wrangler.mjs     # wrangler 設定を生成（変数はワークフロー参照）
   npx wrangler secret put GITHUB_CLIENT_SECRET --config deploy/cloudflare/wrangler.staging.generated.toml
   npx wrangler secret put CREDENTIAL_KEYS      --config deploy/cloudflare/wrangler.staging.generated.toml
   ```
5. AWS S3 バケットも扱う場合（`enable_aws_sts_principal = true`）:
   ```bash
   aws iam create-access-key --user-name ccpr-staging-cf-registry-sts
   npx wrangler secret put REGISTRY_AWS_ACCESS_KEY_ID     --config ...
   npx wrangler secret put REGISTRY_AWS_SECRET_ACCESS_KEY --config ...
   ```
   アクセスキーは 90 日ごとにローテーション（新キー作成 → シークレット更新 → 旧キー無効化 → 削除）。

### 1.5 AWS 版

1. `infra/aws/envs/<env>.tfvars` を埋める（`github_client_id`、必要ならカスタムドメインと ACM 証明書）。
2. `Deploy (AWS)` ワークフローを実行。初回は Secrets Manager にシークレットの値がないため、Lambda は起動時エラーになります（fail closed）。
3. シークレット値を登録:
   ```bash
   aws secretsmanager put-secret-value --secret-id ccpr-staging/registry \
     --secret-string file://<(jq -n --arg s "$GITHUB_CLIENT_SECRET" --arg k "$CREDENTIAL_KEYS" \
       '{GITHUB_CLIENT_SECRET:$s, CREDENTIAL_KEYS:$k}')
   ```
4. Lambda は実行環境の初回リクエスト時にシークレットを読みます。読み込みに失敗した実行環境は次のリクエストで再試行するため、初回登録はそのまま反映されます。**値を変更（ローテーション）した場合**は既存の実行環境が古い値を保持しているので、再デプロイなどで実行環境を入れ替えてください。
5. カスタムドメインを使う場合は `custom_domain_target` を CNAME 先に設定します。

## 2. 通常のデプロイ

- `main` への変更は CI（lint / typecheck / unit / integration / terraform validate）が通っていること。
- Actions → `Deploy (Cloudflare)` / `Deploy (AWS)` → 環境を選択して実行。
  1. plan ジョブ（`<env>-plan` 環境）で `terraform plan` を作成しアーティファクト化
  2. `<env>` 環境で承認 → 同一ジョブで plan の apply、（Cloudflare は D1 マイグレーション、Worker デプロイ）、スモークテスト
- スモークテストに認証付きの確認を含めるには、テスト用 GitHub ユーザーの読み取り専用トークン（`read:org`）を `SMOKE_GITHUB_TOKEN` に登録します。

## 3. ロールバック

- **Cloudflare Worker**: `npx wrangler rollback --config deploy/cloudflare/wrangler.<env>.generated.toml`（直前のバージョンへ）、または `wrangler deployments list` で確認して指定。
- **Lambda**: 前回の成功コミットでワークフローを再実行（zip が再ビルドされ `terraform apply` で戻ります）。
- **D1 スキーマ**: マイグレーションは前方互換で追加する方針。戻す必要がある場合は新しいマイグレーションで打ち消します。
- **Terraform**: 前回のコミットで plan/apply。`prevent_destroy`（D1）と `deletion_protection_enabled`（本番 DynamoDB）により誤削除は止まります。

## 4. バックアップと復旧

| 対象 | バックアップ | 復旧 |
| --- | --- | --- |
| D1 | D1 Time Travel（任意時点に復元。Workers Paid は 30 日、Free は 7 日）。長期保管は `wrangler d1 export <db> --remote --output backup.sql` を定期実行 | `wrangler d1 time-travel restore <db> --timestamp <ISO8601>`、または新 DB に `wrangler d1 execute --file backup.sql` |
| DynamoDB | PITR（35 日）有効 | `aws dynamodb restore-table-to-point-in-time` で新テーブルに復元 → Terraform の `TABLE_NAME` を切替、または import |
| シークレット | Secrets Manager はバージョン管理・削除猶予 30 日。Worker シークレットは読み出し不可のため、**鍵（`CREDENTIAL_KEYS`）は別途安全な保管庫に保管**すること | 再登録 |
| ユーザーのデータ | ユーザー所有の R2/S3 にあり Registry の管理外 | — |

**`CREDENTIAL_KEYS` を失うと、保存済みの R2 資格情報とセッションは復号できなくなります。** その場合は全ユーザーに `PUT /v1/buckets/{id}/credentials` での再登録を依頼します（S3 はロール方式なので影響なし）。

## 5. 定常作業

| 作業 | 方法 |
| --- | --- |
| 暗号鍵のローテーション | 新しい鍵を先頭に追加（`knew:...,kold:...`）→ デプロイ。新規の暗号化は新鍵、既存データは旧鍵で復号可能。資格情報の再暗号化は所有者による再登録時に行われます（一括再暗号化ツールは未実装） |
| GitHub client secret のローテーション | OAuth App で新しい secret を発行 → シークレット更新 → 旧 secret 削除 |
| 期限切れセッション等の削除 | Cloudflare: Cron（毎日 03:17 UTC）で自動。AWS: DynamoDB TTL で自動 |
| 依存関係の更新 | `npm audit --omit=dev` が 0 件であること。SeaweedFS イメージを更新する場合はタグと digest を同時に更新し `npm run test:integration` を実行 |
| コスト確認 | AWS Budget 通知、Cloudflare ダッシュボード（Workers リクエスト数、D1 読み書き） |

## 6. 障害・インシデント対応

| 状況 | 対応 |
| --- | --- |
| 特定ユーザーのセッションを無効化したい | D1: `DELETE FROM sessions WHERE user_id = ?`／DynamoDB: 該当 `SESSION#` 項目を削除。GitHub 側の OAuth 許可取り消しも案内 |
| 全セッションを無効化したい | D1: `DELETE FROM sessions`／DynamoDB: `SESSION#` 項目を削除。または `CREDENTIAL_KEYS` に新鍵のみを設定（R2 資格情報も復号不能になるため最終手段） |
| 問題のあるバケットを止めたい | 所有者に `DELETE /v1/buckets/{id}` を依頼。緊急時は DB で `verify_status = 'failed'` に更新すると一覧・ダウンロードが止まります |
| GitHub 障害・レート制限 | Org/Team 経由のアクセスは拒否されます（仕様）。所有者・Public・User ACL は Cookie セッション中なら継続。復旧後は自動回復 |
| 5xx の増加 | CloudWatch Logs / Workers Observability でリクエスト ID から追跡。設定エラー（`ENVIRONMENT`、シークレット未設定）は全リクエストが 500 になります |
| 攻撃・大量アクセス | WAF のレート制限値を下げる、Cloudflare は「Under Attack」モード、AWS は WAF に IP セットのブロックルールを追加 |
| 監査ログの確認 | D1: `SELECT * FROM audit_log WHERE bucket_id = ? ORDER BY ts DESC`／DynamoDB: `PK = AUDIT#<日付>` を Query。アプリログにも `msg: "audit"` で出力 |

## 7. バケット所有者向け手順

### Cloudflare R2

1. R2 バケットを作成（**Public access / r2.dev / カスタムドメインは無効のまま**にしてください。Private 登録時にこれらで公開されていると ACL を迂回されます）。
2. R2 → Manage R2 API Tokens → **Object Read only**、対象を**そのバケットのみ**に限定したトークンを作成。
3. Registry に登録:
   ```json
   POST /v1/buckets
   {"name":"my-maps","visibility":"private",
    "storage":{"provider":"r2","accountId":"<32桁のアカウントID>","bucket":"<bucket>"},
    "credentials":{"accessKeyId":"...","secretAccessKey":"..."}}
   ```
4. `POST /v1/buckets/{id}/verify` → `PUT /v1/buckets/{id}/acl`。

### AWS S3

1. `POST /v1/buckets` に `{"storage":{"provider":"s3","region":"...","bucket":"...","roleArn":"arn:aws:iam::<自分のアカウント>:role/<ロール名>"}}` を送り、レスポンスの `awsTrust.principalArn` と `awsTrust.externalId` を控えます。
2. 自分のアカウントにロールを作成:
   - 信頼ポリシー:
     ```json
     {"Version":"2012-10-17","Statement":[{"Effect":"Allow",
       "Principal":{"AWS":"<awsTrust.principalArn>"},
       "Action":"sts:AssumeRole",
       "Condition":{"StringEquals":{"sts:ExternalId":"<awsTrust.externalId>"}}}]}
     ```
   - 権限ポリシー（読み取りのみ）:
     ```json
     {"Version":"2012-10-17","Statement":[
       {"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::<bucket>/*"},
       {"Effect":"Allow","Action":"s3:ListBucket","Resource":"arn:aws:s3:::<bucket>"}]}
     ```
   - KMS（SSE-KMS）で暗号化している場合は `kms:Decrypt` も必要です。
3. S3 の「パブリックアクセスをすべてブロック」を有効のままにします。
4. `POST /v1/buckets/{id}/verify`。ExternalId を変えたい場合は `PUT /v1/buckets/{id}/credentials` に `{"roleArn":"...","rotateExternalId":true}`。

### 登録解除

`DELETE /v1/buckets/{id}` は Registry 上のメタデータと ACL のみを削除します。**ストレージ上のファイルは削除されません。** 不要になった R2 トークン / IAM ロールは所有者が削除してください。

## 8. CLI のリリース（PyPI）

`cli/` の Python パッケージ `carla-pack-registry`（コマンド `carla-pack`）は、PR のラベルでリリースします（[simple_lanelet2](https://github.com/hakuturu583/simple_lanelet2) と同じ仕組み、`.github/workflows/release.yml`）。

| ラベル | main にマージしたとき |
| --- | --- |
| `release:patch` | 0.1.0 → 0.1.1 |
| `release:minor` | 0.1.0 → 0.2.0 |
| `release:major` | 0.1.0 → 1.0.0 |
| なし | 何もしない |

ラベルは 1 つだけ付けます（複数あるとリリースジョブが失敗します）。マージされると同じワークフロー実行の中で次が順に走ります。

1. `scripts/bump-cli-version.py` で `cli/Cargo.toml` と `cli/Cargo.lock` のバージョンを上げ、`chore(release): vX.Y.Z` を main にコミット、`vX.Y.Z` タグと GitHub Release（自動生成ノート）を作成
2. タグのコミットで fmt / clippy / `cargo test` を再実行し、タグとパッケージのバージョンの一致を確認
3. Linux x86_64 / aarch64（manylinux 2_28）、macOS arm64 / x86_64、Windows x64 の wheel（abi3、Python 3.10 以上）を各アーキテクチャのランナーでビルドし、クリーンな venv にインストールしてテスト。sdist はビルドしてから一度インストールして確認
4. `twine check --strict` の後、PyPI に Trusted Publishing でアップロード（`skip-existing`。失敗した分は再実行で追加できます）

バージョンの唯一の定義は `cli/Cargo.toml` の `[package] version` です（`pyproject.toml` は dynamic）。手で変えず、ラベルか `scripts/bump-cli-version.py` を使ってください。タグとバージョンは CLI のもので、Registry サービスのデプロイ（§2）とは独立しています。

### 初回だけ必要な設定

1. PyPI（https://pypi.org/manage/account/publishing/）で **pending publisher** を追加します: PyPI project name `carla-pack-registry`、owner `hakuturu583`、repository `carla_content_pack_registry`、workflow `release.yml`、environment `pypi`。
2. GitHub の Settings → Environments に `pypi` を作ります（必要なら Required reviewers を付けると、公開前に承認を挟めます）。

設定前に release ラベルの PR をマージすると、タグと GitHub Release までは作られ、publish ジョブだけが失敗します。wheel と sdist はその実行の artifact に残るので、設定後に「Re-run failed jobs」で公開できます。

### その他の入口

- **試しにビルドする**: Actions → Release → Run workflow（`workflow_dispatch`）。wheel と sdist を作ってテストするだけで、タグも公開もしません。`release.yml`、`scripts/bump-cli-version.py`、`cli/Cargo.*`、`cli/pyproject.toml` などを変更する PR でも同じドライランが走ります。
- **手でタグを切る（hotfix など）**: バージョンを `scripts/bump-cli-version.py X.Y.Z` で上げてコミットし、`git tag -a vX.Y.Z && git push origin vX.Y.Z`。タグのプッシュでビルドと公開だけが走ります（タグとパッケージのバージョンが違うと止まります）。
