# セキュリティ

## 信頼の境界

| 何を信じるか | 理由 |
| --- | --- |
| GitHub のリポジトリ権限 | 「Pack リポジトリを読める人 = 取得してよい人」と定義している。マニフェストも鍵もこの権限でしか読めない |
| Pack リポジトリへの書き込み権限を持つ人 | マニフェスト（どこから・どの SHA-256 で取るか）と鍵の workflow を変更できる。リポジトリの Write 権限は Pack の公開者に限る |
| ストレージの鍵の権限 | 一時鍵は `object-read-only`（R2）/ セッションポリシーで `s3:GetObject`（AWS）、いずれもプレフィックス限定 |

利用者側は、マニフェストの SHA-256・サイズと、Pack 自身の `carla-pack.json` の全ファイルのハッシュを検証してから配置します。
ストレージ側のファイルが差し替えられても、マニフェストと一致しなければ何もインストールしません。

## 対策の一覧

| 脅威 | 対策 | 確認 |
| --- | --- | --- |
| 権限のない人の取得 | マニフェストと鍵は GitHub API でしか取れず、読めなければ 404。ストレージは鍵なしでは拒否 | `tests/flow.rs` `no_read_access_means_no_pack`、`python/tests/test_e2e.py` |
| GitHub トークンの漏洩（ストレージ側へ） | トークンは GitHub API のホストにだけ送る。asset のリダイレクト先（別ホスト）には `Authorization` を付けない（reqwest がオリジンをまたぐリダイレクトで除去）。S3 には SigV4 だけ | `tests/flow.rs`（リダイレクト先で Authorization なしを要求）、E2E の `leaked == []` |
| ストレージ上のファイルの改ざん・取り違え | マニフェストの SHA-256 / サイズを照合。不一致なら一時ファイルを削除しエラー（`checksum_mismatch`） | `tampered_downloads_are_discarded` |
| マニフェストと中身の食い違い | 展開後の `carla-pack.json` の名前・バージョン・base release・platform がマニフェストと一致しなければ拒否 | `tests/pack.rs` |
| 悪意ある tarball（`..`、絶対パス、リンク、未記載ファイル、重複メンバー） | 単一トップフォルダの通常ファイルとディレクトリのみ受け付け、全ファイルを manifest と照合。失敗時は packs_dir に何も残さない | `tests/pack.rs` |
| 公開リポジトリから鍵を配ってしまう | 鍵の workflow はリポジトリが Private でなければ失敗する | `templates/pack-repo/.github/workflows/*` |
| 鍵の権限が広すぎる | R2 は `object-read-only` + prefix、AWS はセッションポリシーで `s3:GetObject` + prefix。元の権限（R2 の親トークン、AWS のロール）を超えない | — |
| 長期鍵の流出 | 配るのは 12 時間で切れる一時鍵。発行元の秘密（Cloudflare API トークン）は Actions secret、AWS は OIDC で秘密なし | — |
| 期限切れの鍵 | 期限まで 2 分未満の鍵は使わず、原因（workflow が止まっている）を示すエラー | `expired_or_missing_credentials_are_reported` |
| エンドポイントの差し替え（SSRF 的な誘導） | `endpoint` は https のオリジンのみ（http は localhost だけ）、パス・クエリ・ユーザー情報を禁止。キーは正規化された相対パスのみ | `src/manifest.rs` のテスト |
| Hugging Face 上の公開 Pack | トークン（GitHub・Hugging Face とも）を一切送らない匿名取得。リポジトリ（`main` など）の中身が書き換えられても、マニフェストの SHA-256 と一致しなければインストールしない。公開リポジトリなので、置いた Pack は誰でも取得できる前提 | `public_packs_come_from_hugging_face_without_any_token` |
| ローカルの資格情報 | 保存するトークンは `config.json`（600）。ストレージの鍵はメモリにだけ置き、ログやエラーに出さない（`Debug` でも秘匿） | `src/sigv4.rs` のテスト |

## 失効と反映の遅延

- **人を外す**: Pack リポジトリ（または Team）から外せば、以後マニフェストも新しい鍵も取れません。
  ただし **すでに取得した鍵は期限（最大 12 時間）まで有効** です。旧版の署名付き URL（5 分）より長くなります。
- **すぐに止めたい**: 鍵の元を無効にします。
  - R2: 一時鍵の親になっている R2 API トークンを削除 / ロール（ローテーション）します。一時鍵は親の権限を超えられないので、そこから作った一時鍵も使えなくなる想定です（**実環境では未確認**。運用前に確かめてください）。
    新しい親トークンの access key ID を `R2_PARENT_ACCESS_KEY_ID` に設定して workflow を手動実行してください。
  - AWS: IAM ロールの「Revoke active sessions」で、それ以前に発行したセッションを無効にできます。
- 期限は workflow の `TTL_SECONDS` / `role-duration-seconds` で短くできます（その場合は cron も短く）。

## 残留リスク・既知の制限

- **鍵はリポジトリを読める全員が取得でき、他人に渡すこともできます。** 渡された人は期限まで、そのプレフィックスの Pack をすべて読めます。
  旧版の署名付き URL と同じ性質ですが、範囲（プレフィックス全体）と期間（12 時間）が広くなっています。
  共有範囲の違う Pack は、リポジトリとプレフィックス（R2 なら別の親トークンでも可）を分けてください。
- **監査ログは粗くなります。** GitHub は Release asset のダウンロード者を記録しません。ストレージのアクセスログには一時鍵の ID しか残りません。
- **トークンの権限**: `gh auth token` や OAuth App のトークン（`repo` スコープ）は、Pack リポジトリ以外にも広い権限を持ちます。
  carla-pack が使うのは読み取りだけですが、専用にするなら GitHub App（Contents: read-only）か fine-grained PAT（対象リポジトリの Contents: read）を使ってください。
- **鍵の置き換えの瞬間**: `gh release upload --clobber` は削除してからアップロードするため、ごく短い間 asset が無くなります。その間の取得は「再試行してください」というエラーになります。
- **R2 の公開アクセス**: バケットに r2.dev やカスタムドメインの公開アクセスを設定すると、この仕組みを迂回して誰でも読めます。Private 運用では無効にしてください。
- Pack リポジトリの Write 権限を持つ人（とそのワークフロー）は、マニフェストを書き換えて別のファイルを指させることができます。SHA-256 で固定した lock ファイルを使う利用者は影響を受けません。
