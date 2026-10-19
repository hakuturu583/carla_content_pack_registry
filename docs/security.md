# セキュリティ

## 指示書 §6 への対応

| 要件 | 対応 | 確認方法 |
| --- | --- | --- |
| OAuth の state 検証、安全なセッション Cookie、CSRF 対策 | state + PKCE（S256）、state は HttpOnly Cookie と定数時間比較。セッション Cookie は `__Host-`、HttpOnly、Secure、SameSite=Lax。Cookie セッションの変更系リクエストは `X-CSRF-Token` と Origin を検証。ログイン後のリダイレクトは同一サイトの相対パスのみ | `test/unit/auth.test.ts` |
| GitHub 数値 ID を内部識別子に使用 | ユーザー・Org・Team すべて数値 ID。名前は表示用のみ | `test/unit/authz.test.ts`（ID 名前空間の混同も検査） |
| Org の OAuth App 承認が必要なケース | メンバーシップ取得が 403 の場合は理由付きで拒否し、`/v1/me` の `membershipError` に表示。Team の名前解決失敗時も承認の可能性を案内 | `test/unit/github.test.ts` |
| GitHub API 失敗・権限不明時は拒否 | `membership_unverifiable` / `upstream_unavailable` を返し、監査ログに deny を記録 | `test/unit/auth.test.ts`、`github.test.ts` |
| ページネーション、キャッシュ、レート制限 | Link ヘッダーで最大 10 ページ（api.github.com 以外の next は無視）。メンバーシップ 5 分キャッシュ、Bearer プロファイル 60 秒キャッシュ。GitHub のレート制限は 503 | `github.test.ts` |
| 資格情報の暗号化、レスポンス・ログ・Terraform state に平文を残さない | AES-256-GCM（HKDF で用途別鍵、AAD でバケット ID / セッションに束縛、鍵ローテーション対応）。API は秘密情報を返さない（所有者にも）。ログは秘密っぽいキーを自動マスク。Terraform はシークレットの「入れ物」だけを作り、値は CLI で投入 | `crypto.test.ts`、`sts.test.ts`、契約テスト（レスポンスに鍵が含まれないこと） |
| SSRF 防止 | ユーザーは URL を一切指定できない。R2 はアカウント ID（32 桁 hex）、S3 はリージョン（正規表現）とバケット名（ドット不可）からエンドポイントを生成。`local` は `ENVIRONMENT=local` のみで、エンドポイントはサーバー設定 | `storage.test.ts`、契約テスト |
| Private バケットの直接公開による ACL 迂回の防止 | 検証時に匿名の一覧・読み取りが拒否されることを確認し、公開されていれば検証失敗（ダウンロード不可） | 契約テスト（`exposed-packs`） |
| WAF による IP / Bot / DDoS 対策 | Cloudflare: カスタムルール、IP レート制限、Bot Fight Mode、（Pro 以上）Managed Ruleset。AWS: WAF（IP レピュテーション、Common、Known Bad Inputs、IP レート制限、任意で Bot Control）を API Gateway REST ステージに関連付け、ステージのスロットリング、Lambda 同時実行上限 | `terraform validate`（実適用は未実施） |
| アプリ層のレート制限 | GitHub ユーザー単位（書き込み 30/分）とユーザー×バケット単位（読み取り 120/分）。Cloudflare は Workers Rate Limiting、AWS は DynamoDB のアトミックカウンタ | `ratelimit.test.ts` |
| 公開環境で Mock 認証を禁止 | 設定読込で拒否（fail closed）、WAF で `/login/mock` をブロック、`/login/mock` ハンドラ自体も二重チェック | `auth.test.ts` |
| ログ・監査証跡・メトリクス・コスト監視 | 構造化 JSON ログ（リクエスト ID、ユーザー ID、ルート、ステータス）、監査ログを D1/DynamoDB に保存（既定 400 日）、CloudWatch アラーム（Lambda エラー/スロットル、API 5xx、WAF ブロック急増）、拒否件数のメトリクスフィルタ、AWS Budget。Cloudflare は Workers Observability | — |

## 署名付き URL についての制約（明記事項）

- 署名付き URL の有効期限は既定で **300 秒**、設定上限も 300 秒です（`PRESIGN_TTL_SECONDS`）。
- **URL は有効期限内であれば誰でも何度でも利用できます。** Registry は発行後の URL を失効させられません。URL を受け取ったクライアントがそれを第三者に渡せば、その第三者も期限内はダウンロードできます。URL は秘密として扱ってください。
- ACL の変更やメンバーシップの喪失は、発行済みの URL には反映されません（最大 300 秒）。
- S3 の場合、URL は STS の一時資格情報（15 分）で署名されます。Registry は残り 6 分未満の一時資格情報を使わないため、URL が期限前に失効することはありません。

## 反映の遅延

- GitHub の Org/Team メンバーシップはユーザーごとに最大 5 分キャッシュされます（`MEMBERSHIP_CACHE_SECONDS`）。Org から外れたユーザーが閲覧できなくなるまで最大 5 分 + 署名付き URL の 5 分かかります。
- セッションは既定 8 時間です。GitHub 側で OAuth 許可を取り消すと、メンバーシップの再取得が失敗するため Org/Team 経由のアクセスは拒否されますが、Public / 本人 / User ACL のバケットはセッション期限まで閲覧できます（[status.md](status.md) 残課題）。

## 残留リスク・既知の制限

- **R2 の公開アクセス（r2.dev、カスタムドメイン）や、バケット前段の CDN は S3 API から検出できません。** 検証時に警告を出しています。Private バケットの所有者はこれらを無効にしておく必要があります。
- Bearer トークンは GitHub が有効と認めるトークンであれば、この OAuth App 以外が発行したもの（PAT など）でも受け付けます。メンバーシップ確認には `read:org` が必要なので、権限が足りないトークンでは Org/Team 経由のアクセスは拒否されます。OAuth App 発行のトークンに限定する（`POST /applications/{client_id}/token` で確認）ことを検討中です。
- Cloudflare 版で AWS S3 を使う場合、Registry は IAM ユーザーの長期アクセスキーを Worker シークレットに保持します（権限は `sts:AssumeRole` のみ）。定期ローテーションが必要です（[runbook.md](runbook.md)）。
- `sts:AssumeRole` の対象は既定で全アカウントの全ロールです。ユーザーのロール信頼ポリシーと ExternalId（Registry 生成、バケットごと）で保護されます。ロール名規約を設けられる場合は `assumable_role_arn_pattern` を絞ってください。
- AssumeRole の際は、ユーザーのロールが広い権限を持っていても、セッションポリシーで `s3:GetObject` / `s3:ListBucket`（対象バケットのみ）に制限します。
- GitHub Actions のデプロイロールは PowerUserAccess + `ccpr-*` に限定した IAM 権限です。plan 用の読み取り専用ロールへの分離は未実施です。
