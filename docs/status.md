# 実装状況・テスト結果・残課題

最終更新: 2026-10-19（ブランチ `claude/registry-foundation`）

凡例: ✅ 完了・検証済み ／ 🟡 実装済み・ローカル検証のみ（実クラウド未検証）／ ❌ 未実施

## まとめ

- アプリケーションコード、IaC、CI/CD、OpenAPI、テスト、ドキュメントは揃っており、**ローカル環境（SeaweedFS、D1 Local / workerd、DynamoDB Local、Mock 認証）では全機能が動作することを確認済み**です。
- **実クラウドでは何も作成・デプロイしていません**（承認と資格情報が必要なため）。本物の R2・S3・GitHub OAuth App・Workers・Lambda に対する動作は未検証です。
- 起点となるはずだった PoC が見つからなかったため、Phase 0 の監査は実施できていません。

## Phase 別

| Phase | 状態 | 内容 | 検証 |
| --- | --- | --- | --- |
| 0 PoC 監査 | ❌（代替のみ） | PoC が存在せず監査不可。PoC 相当のローカル環境を新規構築し、既知課題（Mock 認証、共通資格情報、平文トークン）が無いことを確認 | [phase0-audit.md](phase0-audit.md) |
| 1 コアの分離 | ✅ | RegistryCore / StorageProvider / RegistryRepository / GitHubIdentityProvider / CredentialResolver を TypeScript で分離。コアはクラウド SDK に非依存 | ユニット 89 件 |
| 2 R2 BYOB | 🟡 | R2 登録、AES-GCM 暗号化、接続検証（公開検出・過剰権限警告）、一覧、署名付きダウンロード | SeaweedFS（同じ S3 API / SigV4 実装）で E2E、R2 エンドポイント・署名形式はユニットテスト。**実 R2 では未検証** |
| 3 GitHub ACL | 🟡 | OAuth（state + PKCE）、セッション、CSRF、User / Org / Team ACL（数値 ID）、所有者限定の管理操作、取得失敗時の拒否 | GitHub API をモックしたユニットテスト、Mock 認証で全 ACL パターンの E2E。**本物の OAuth App / GitHub API では未検証** |
| 4 Cloudflare 本番対応 | 🟡 | Terraform（D1、DNS、WAF、レート制限、Bot）、wrangler 設定テンプレート、デプロイワークフロー、Cron によるクリーンアップ | `terraform validate` 成功、`wrangler deploy --dry-run` 成功（137 KiB）、workerd 上でスモークテスト成功。**apply / デプロイ / staging E2E は未実施** |
| 5 AWS S3 対応 | 🟡 | STS AssumeRole、Registry 生成の ExternalId、読み取り専用セッションポリシー、一時資格情報キャッシュ、ExternalId ローテーション | STS をモックしたユニットテスト。**実 AWS では未検証** |
| 6 AWS Registry 対応 | 🟡 | Lambda ハンドラ、DynamoDB 単一テーブル、DynamoDB レート制限、Secrets Manager、Terraform（API Gateway REST、WAF のステージ関連付け、KMS、CloudWatch、Budget）、OIDC bootstrap | Lambda ハンドラを API Gateway REST 形式のイベント + DynamoDB Local + SeaweedFS で E2E（Cloudflare 版と同一の契約テスト 18 シナリオ）。`terraform validate` 成功。**apply / デプロイは未実施** |
| CLI `carla-pack` | 🟡 | Rust コア + Python フロントエンド（PyO3 / maturin、abi3 wheel）。`login`（GitHub デバイスフロー / トークン）、`logout`、`whoami`、`ls`、`pull`、`bucket add/show/set/credentials/verify/rm`、`acl show/add/rm/set`、`--json` | Rust テスト 16 件（Registry・ストレージ・GitHub はモックサーバー）、CLI E2E 3 件（wrangler dev + SeaweedFS、Mock 認証）。**本物の GitHub デバイスフローは未検証**。アップロード（`push`）は未実装 |
| 7 品質・運用 | 🟡 一部 | CI、監査ログ、構造化ログ、Runbook、バックアップ/復旧手順、OpenAPI と実装の同期テスト | セキュリティテストはユニット/契約テストの範囲。ペネトレーションテスト、負荷試験、ダッシュボードは未実施 |

## テスト結果（このセッションでの実行）

| 種別 | コマンド | 結果 |
| --- | --- | --- |
| 型検査 | `npm run typecheck` | 成功 |
| Lint | `npm run lint` | 0 件 |
| ユニット + 契約（Memory / D1 SQLite / Fake S3） | `npm test` | 92 / 92 成功 |
| 統合（Worker エントリ + D1 + SeaweedFS、Lambda ハンドラ + DynamoDB Local + SeaweedFS） | `npm run test:integration` | 36 / 36 成功 |
| wrangler dev（workerd + D1 Local + SeaweedFS）スモーク | `npm run smoke` | 12 / 12 成功 |
| CLI Rust コア | `cd cli && cargo test`（+ `cargo clippy -D warnings`、`cargo fmt --check`） | 16 / 16 成功 |
| CLI E2E（wrangler dev に対して） | `cd cli && pytest` | 3 / 3 成功。release wheel を Python 3.12 のクリーンな venv に入れて動作確認 |
| ブラウザ UI（Chromium） | Playwright（手動スクリプト） | Mock ログイン → ACL に応じた一覧 → ダウンロード → ログアウト成功 |
| OpenAPI | `redocly lint` | エラー・警告 0 |
| Terraform | `terraform fmt` / `validate`（cloudflare, aws, aws/bootstrap） | 成功（※後述の制約） |
| 本番依存の脆弱性 | `npm audit --omit=dev` | 0 件 |

契約テストのシナリオ（`test/helpers/scenarios.ts`）: 未認証拒否、`/v1/me`、登録と秘密情報の非返却、SSRF/入力検証、未検証時の 409、ACL 書き込み（名前 → 数値 ID、重複排除、不明な Org は 400）、検証、ユーザーごとの一覧（alice / bob / carol）、閲覧 ACL（user / org / team / 存在の非開示）、プレフィックス・ページング、署名付き URL（有効期限 300 秒、スペースやスラッシュを含むキー、302 モード）、所有者限定操作（403/404）、Private 化での再検証、公開バケットの検出、他人のバケットを自分のキーで登録した場合の検証失敗、登録解除でストレージが残ること。

## 受け入れ条件（§10）の状況

| Registry | ストレージ | 状態 |
| --- | --- | --- |
| Cloudflare Workers | Cloudflare R2 | 🟡 未検証（ローカル: workerd + SeaweedFS で同等フロー成功） |
| Cloudflare Workers | AWS S3 | 🟡 未検証（STS 部分はモックのみ） |
| AWS Lambda | Cloudflare R2 | 🟡 未検証（ローカル: Lambda ハンドラ + DynamoDB Local + SeaweedFS で同等フロー成功） |
| AWS Lambda | AWS S3 | 🟡 未検証（STS 部分はモックのみ） |
| ローカル（SeaweedFS） | — | ✅ 再現可能（`README.md` の手順、CI の integration ジョブ） |

実クラウド検証には次の承認・情報が必要です: Cloudflare アカウント/ゾーン、AWS アカウント、環境ごとの GitHub OAuth App、テスト用の R2 バケットと S3 バケット（＋ロール）、staging 用 GitHub テストユーザーと Org/Team。

## 残課題

### 優先度高

1. **PoC の取り込み**: 元の `registry-local-prototype-seaweedfs` の所在が分かれば、機能・データの差分を確認して取り込む。
2. **実クラウドでの検証**（上記 4 パターン）と staging E2E。
3. **API Gateway REST でのキーのエンコード**: `%2F` を含むパスが API Gateway REST でどう渡るか未確認。クライアントにはセグメント単位のエンコード（`/` はそのまま）を推奨しており、UI とスモークテストもその形式。staging で確認する。
4. **Bearer トークンの発行元**: 他アプリのトークンや PAT も受け付ける。OAuth App 発行トークンへの限定（`POST /applications/{client_id}/token`）を検討。
5. **セッション中の GitHub 許可取り消し**: Org/Team 経由は拒否されるが、Public / 本人 / User ACL はセッション期限（8 時間）まで閲覧可能。定期的な `/user` 再検証を追加するか検討。

### 中

6. `GET /v1/buckets` は候補バケットごとに ACL を読む（N+1）。候補は最大 1000 件、D1 では ACL 照合に使う主体を 90 件までに制限（それを超える Org/Team 所属ユーザーは一覧に出ないことがある。個別取得の認可は正確）。大規模化の前にページングと集約クエリを追加。
7. 暗号鍵ローテーション後の既存資格情報の一括再暗号化ツール。
8. plan 用の読み取り専用 AWS デプロイロール（現在は apply と同じロール）。
9. Terraform の lock ファイルは linux_amd64 のハッシュのみ（`terraform providers lock -platform=...` で追加）。このセッションでは registry.terraform.io に接続できず、プロバイダはリリースサイトから取得して検証した。
10. GitHub Actions のアクションはメジャータグ指定。コミット SHA への固定を推奨。
11. 開発依存の advisory（vitest 3 系）。vitest 4 への更新が npm 10.9 の依存解決エラーで失敗したため保留。
12. 監査ログの参照 API（運営者向け）は無い。DB とログ基盤から参照する運用。
13. CLI の残り: アップロード（`push`。署名付き PUT を Registry が発行するか、ユーザー自身の資格情報で直接置くかの方針決定が必要）、Content Pack のマニフェスト／バージョン管理、OS キーチェーンへのトークン保存、PyPI 公開（trusted publishing の設定が必要。wheel のビルドは `.github/workflows/cli-wheels.yml`）。

### 指示書 §11（実装対象外）

Git LFS Batch API 互換、Content Pack 間の依存解決、Org によるバケット共同所有、Custom Group、所有権の譲渡、ファイル単位 ACL、大規模検索・分析は実装していません。
