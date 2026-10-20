# Phase 0 — PoC 監査

## 1. 起点 PoC の所在

指示書では既存の `registry-local-prototype-seaweedfs` を起点とするよう指定されていましたが、作業開始時点で次のいずれにも存在しませんでした。

| 確認箇所 | 結果 |
| --- | --- |
| `hakuturu583/carla_content_pack_registry`（本リポジトリ、全ブランチ） | `README.md` 1 行のみの初期コミット |
| 作業コンテナのファイルシステム | 該当ディレクトリなし |
| GitHub `hakuturu583/registry-local-prototype-seaweedfs` | 存在しない、またはこのセッションの GitHub 資格情報からアクセス不可 |

このため **PoC のコード監査・起動確認は実施できていません**。Phase 0 の代替として、指示書に記載された PoC の構成（Wrangler dev + D1 Local + SeaweedFS + Mock 認証）を仕様から新規に構築し、指示書に挙げられた PoC の既知の問題点がこの実装に持ち込まれていないことを確認しました。

**PoC が別の場所にある場合は、リポジトリ名かアーカイブを共有してください。** 既存 PoC にあった機能・データ（UI、CLI、シードデータなど）との差分を洗い出し、必要なものを取り込みます。

## 2. 指示書に記載された PoC の既知課題と、この実装での扱い

| PoC の課題 | この実装 |
| --- | --- |
| Mock 認証 | `AUTH_MODE=mock` は `ENVIRONMENT=local` でしか起動できません（`src/runtime/config.ts`）。それ以外では設定エラーとして全リクエストを拒否します（テスト: `test/unit/auth.test.ts`）。さらに Cloudflare WAF / AWS WAF で `/login/mock` をブロックします。 |
| 共通の開発用 S3 資格情報 | SeaweedFS にはユーザーごとに別のアクセスキーとバケット権限を設定（`local/seaweedfs/s3.json`）。Registry 側にも共通資格情報はなく、バケットごとに所有者が登録したキーを暗号化して保持します。他人のバケットを自分のキーで登録しても検証に失敗することをテストしています。 |
| 暗号化されていない OAuth トークン保存 | GitHub トークンは AES-256-GCM で暗号化し、セッション ID（のハッシュ）に紐付けた AAD で保存します。セッション Cookie はランダム値で、DB にはその SHA-256 のみを保存します。 |

## 3. ローカル環境（PoC 相当）の起動確認

| 項目 | 結果 |
| --- | --- |
| SeaweedFS `chrislusf/seaweedfs:3.97`（digest 固定） | 起動・S3 API・ユーザー別権限・匿名拒否・署名付き URL を確認 |
| `wrangler dev`（workerd）+ D1 Local + SeaweedFS + Mock 認証 | スモークテスト（登録 → 検証 → 一覧 → 署名付き URL → 直接ダウンロード → 登録解除）全項目成功 |
| ブラウザ UI（Chromium / Playwright） | Mock ログイン（bob）→ Org ACL のバケットのみ表示 → 一覧 → ダウンロード → ログアウトを確認 |

## 4. 依存関係とライセンス

### 実行時に含まれるもの

| パッケージ | バージョン | ライセンス | 用途 |
| --- | --- | --- | --- |
| hono | 4.13.13 | MIT | HTTP ルーティング（Workers / Lambda 共通） |
| aws4fetch | 1.0.20 | MIT | SigV4 署名（S3 / R2 / STS）、署名付き URL |
| @aws-sdk/client-dynamodb, lib-dynamodb, client-secrets-manager | 3.1153.0 | Apache-2.0 | AWS 版のみ（Lambda バンドルに同梱） |

`npm audit --omit=dev`: **0 件**（hono 4.10 系にあった Lambda アダプタ / Cookie / bodyLimit 関連の advisory は 4.13.13 への更新で解消）。

### 開発・テスト用

| パッケージ | ライセンス |
| --- | --- |
| wrangler 4.151.2 | MIT OR Apache-2.0 |
| typescript 5.9, eslint 9, typescript-eslint 8, esbuild 0.25 | Apache-2.0 / MIT |
| vitest 3.2.4 | MIT |
| @cloudflare/workers-types, @types/* | MIT / MIT OR Apache-2.0 |

開発依存に LGPL-3.0 の `@img/sharp-libvips-*`（wrangler → miniflare → sharp の任意依存）が含まれますが、配布物（Worker / Lambda バンドル）には含まれません。
`npm audit`（開発依存込み）では vitest 3 系の依存（`@vitest/mocker`, `tinypool`, `braces`）に advisory が残っています。vitest 4 への更新は npm 10.9 の依存解決エラー（`Cannot read properties of null (reading 'edgesOut')`）で失敗したため見送りました（[status.md](status.md) の残課題）。

### コンテナイメージ・外部ツール

| 名前 | バージョン | ライセンス | 用途 |
| --- | --- | --- | --- |
| SeaweedFS | 3.97（digest 固定） | Apache-2.0 | ローカル S3 互換ストレージ（MinIO は不使用） |
| DynamoDB Local | 2.5.4 | Amazon の DynamoDB Local ライセンス | AWS 版リポジトリの統合テストのみ |
| Terraform | 1.13.4 | BUSL-1.1 | IaC（ツールとして使用） |
| Terraform providers: hashicorp/aws 6.17, cloudflare/cloudflare 5.11 | | MPL-2.0 | |
