# アーキテクチャ

## 考え方

「この人はこの Pack をダウンロードしてよいか」を判断する主体を、自前のサーバから **GitHub のリポジトリ権限** に移しました。

- Pack リポジトリ（GitHub、Private）を読める ⇔ その Pack を取得できる。
- そのために必要な情報（どこに・どの SHA-256 で置いてあるか、読み取り用の鍵）は、すべてリポジトリの中に置く。
- クライアントは利用者自身の GitHub トークンでそれを読み、ストレージから直接ダウンロードする。

中央のサーバ・データベース・秘密情報の保管場所はありません。

## 流れ

```
利用者の carla-pack / Python                 GitHub                          S3 互換ストレージ
──────────────────────────                  ──────                          ────────────────
1. GET /repos/{o}/{r}/contents/carla-packs.toml ─► 読めなければ 404（終了）
   ← マニフェスト（storage と [[pack]]）
2. 参照（OWNER/REPO/PACK@VER）またはマップ名から
   リリースを 1 つに決める（新しい順、曖昧ならエラー）
3. GET /repos/{o}/{r}/releases/tags/carla-pack-credentials
   GET .../releases/assets/{id}  ─► 302（別ホストの署名付き URL。トークンは送らない）
   ← credentials.json（読み取り専用・期限付き）
4. GET {endpoint}/{bucket}/{prefix}{key}  （SigV4、UNSIGNED-PAYLOAD）─────────────────► 鍵の権限で判定
   ← tarball（ストリームで SHA-256 を計算しながら保存。切断されたら Range: bytes=N- で続きから）
5. サイズと SHA-256 をマニフェストと照合 → キャッシュ（blobs/<sha256>.tar.gz）
6. 展開して carla-pack.json の全ファイルを照合、名前・バージョン・base release・platform がマニフェストと一致することを確認
7. <packs_dir>/<name> に配置 → （任意）carla.Client.mount_content_pack
```

鍵を作る側（Pack リポジトリの GitHub Actions、6 時間ごと）:

```
R2:  Cloudflare API（secret の API トークン）→ POST /accounts/{id}/r2/temp-access-credentials
     （object-read-only、prefix 限定、12 時間）
AWS: GitHub OIDC → sts:AssumeRoleWithWebIdentity（12 時間、セッションポリシーで s3:GetObject と prefix に限定）
→ credentials.json を Release "carla-pack-credentials" の asset として置き換える（Private でなければ中止）
```

## 形式

### `carla-packs.toml`（Pack リポジトリのルート）

```toml
schema = 1

[storage]
endpoint = "https://<account>.r2.cloudflarestorage.com"   # https のみ（http は localhost だけ）。パスなしのオリジン
region = "auto"                                          # 既定 auto（R2）。S3 はバケットのリージョン
bucket = "carla-packs"
prefix = "packs/"                                        # 任意。"/" で終わる相対パス
addressing = "path"                                      # path（既定）| virtual
credentials = "github-release"                           # github-release | anonymous（公開バケット）
credentials_tag = "carla-pack-credentials"               # 任意
# type = "s3" は省略可（既定）。公開 Pack を Hugging Face に置く場合:
# type = "huggingface" / repo = "OWNER/NAME" / repo_type = "dataset" | "model" / revision = "main" / prefix

[[pack]]
name = "NishishinjukuPack"            # CARLA の Pack 名（英字始まりの英数字と _）
version = "1.2.0"
base_release = "carla-0.10.0-Linux"
platform = "Linux"
key = "NishishinjukuPack-1.2.0-carla-0.10.0-Linux.tar.gz"   # prefix からの相対キー
sha256 = "…"                          # tarball の SHA-256（小文字 16 進 64 桁）
size = 3221225472
maps = ["NishishinjukuMap"]
description = "…"                     # 任意
yanked = false                        # 任意。true にすると選ばれない（sha256 で固定した lock からは取得可）

[pack.formats]                        # 任意。マップごとの地図データ形式（publish が書く。下の「地図データの形式」）
NishishinjukuMap = ["autoware", "lanelet2", "navmesh", "opendrive", "sumo"]
```

- 未知のキーはエラーにします（タイプミスの見逃し防止）。
- `(name, version, base_release, platform)` の重複はエラー。`carla-pack publish` は既存のものを置き換えず追記だけします。
- 選択: 条件（名前・バージョン・base release・platform・sha256）に合うもののうち最新版。最新版が複数の base release / platform にあれば曖昧としてエラー。
  バージョンは区切り（`.` `-` `+` `_`）ごとに数値比較し、`1.0.0` は `1.0.0-rc1` より新しいとみなします。
- マップ名は `/Game/.../Town01` のようなパスでも末尾の名前で照合します。
- `formats` はマップ名 → 形式名の一覧です。`find --format` はこの索引で探し、`find --assets` はここにだけ現れるマップ名（CARLA のレベルを持たないデータだけのマップ）も対象にします。
  `formats` を書かずに公開した過去のリリースは、形式では見つからず、マップ名でだけ見つかります（取得後の `carla-pack formats` は過去のリリースでも使えます）。
  `formats` を含むマニフェストは、このキーを知らない古い carla-pack（`formats` 追加前）では読めません。

### 地図データの形式

Pack が持つ地図データを、マップ名と形式名で引けます。CARLA サーバは要りません。

- **標準の形式**: どの Pack でも、`carla-pack.json` の各マップの `xodr` / `nav` / `tm` / `map_logic` から作ります。形式名は `opendrive` / `navmesh` / `traffic_manager`（フォルダ）/ `map_logic`。
- **asset ファイル（任意）**: Pack が `Content/Config/roadgen_asset.json` を持っていれば、その形式も加えます。roadgen の `carla_pack_assets` が書くファイルで、Lanelet2 と点群地図（Autoware）、SUMO、CommonRoad、ClipGT、RDS-HQ、GPUDrive、WOMD などを `Content/HDMaps/<map>/` に入れたことを示します。持たない Pack は標準の形式だけです。同じ形式名は標準の方を使います。

```json
{
  "schema": 1,
  "generator": {"name": "roadgen", "version": "0.43.0"},
  "maps": {
    "Highway": {
      "formats": {
        "lanelet2": {"type": "file", "path": "Content/HDMaps/Highway/autoware/lanelet2_map.osm"},
        "autoware": {"type": "directory", "path": "Content/HDMaps/Highway/autoware",
                     "files": {"lanelet2_map": "…/lanelet2_map.osm", "pointcloud_map": "…/pointcloud_map.pcd",
                               "map_projector_info": "…/map_projector_info.yaml"}}
      }
    }
  }
}
```

- パスはすべて Pack フォルダ（`carla-pack.json` のあるフォルダ）からの正規化された相対パスで、外に出るパスはエラーです。
- ビルド済みの Pack では、asset ファイルが指すファイルはすべて `carla-pack.json` の `files` に載っていなければなりません（インストール時の SHA-256 照合の対象であることの確認）。載っていなければ asset ファイルごと拒否します。
- `schema` が 1 以外ならエラーです。

### `credentials.json`（Release asset）

```json
{
  "version": 1,
  "access_key_id": "…",
  "secret_access_key": "…",
  "session_token": "…",
  "expires_at": "2026-11-03T00:00:00Z"
}
```

`session_token` と `expires_at` は任意（長期鍵の場合は省略）。`expires_at` は RFC 3339（オフセット付きも可）。
期限まで 2 分を切った鍵は使わず、「鍵の workflow が動いていない」というエラーにします。

## コード

| ファイル | 役割 |
| --- | --- |
| `src/github.rs` | アクセス確認: マニフェストと鍵を GitHub API から読む。404 → `no_access` |
| `src/manifest.rs` | `carla-packs.toml` の型・検証、参照 `OWNER/REPO/PACK[@VER]`、リリースの選択 |
| `src/sigv4.rs` | AWS Signature V4（ヘッダ署名、`UNSIGNED-PAYLOAD`）。AWS の公式テストベクタで検証 |
| `src/hf.rs` | Hugging Face の公開リポジトリからの取得（匿名の `resolve` URL、Range で再開） |
| `src/s3.rs` | GET（Range で再開）/ HEAD / PUT、64 MiB 超はマルチパートアップロード（失敗時は abort）。path / virtual-host 形式 |
| `src/client.rs` | `resolve` / `find_map` / `fetch`（キャッシュと検証）/ `install` / `unpack` / `formats` / `map_data` / `publish` |
| `src/pack.rs` | tarball の安全な展開、`carla-pack.json` による全ファイル照合、配置 |
| `src/formats.rs` | 地図データの形式: `carla-pack.json` の標準の形式と `roadgen_asset.json` の形式の読み取り、索引、パスの解決 |
| `src/store.rs` | トークンの探索、保存済み設定（ログイン、検索対象リポジトリ）、キャッシュの場所 |
| `src/auth.rs` | GitHub デバイスフロー（client ID のみ、client secret 不要） |
| `src/python.rs` | PyO3 バインディング（`carla_pack_registry._core`） |
| `python/carla_pack_registry/deploy.py` | `deploy_pack` / `ensure_map`: CARLA サーバへの mount / unmount |
| `python/carla_pack_registry/publish.py` | `publish`: 確認（Rust の `plan_publish`）→ アップロード（S3 は Rust、Hugging Face は `huggingface_hub`）→ 追記（`append_entry`） |
| `python/carla_pack_registry/cli.py` | `carla-pack` コマンド |
| `templates/pack-repo/` | Pack リポジトリ用のマニフェスト雛形と鍵ローテーション workflow |

## 旧 Registry サーバ（Workers / Lambda）との違い

| | 旧: Registry サーバ | 新: GitHub + S3 直接 |
| --- | --- | --- |
| アクセス判定 | サーバが GitHub の Org / Team メンバーシップを引いて ACL を判定 | GitHub がリポジトリの Read 権限で判定（読めなければ 404） |
| 共有の単位 | バケット単位の ACL（user / org / team） | リポジトリ（とバケットのプレフィックス）単位。Team 単位で配るならリポジトリに Team を追加 |
| ダウンロード | サーバが署名した 5 分の URL | 利用者が 12 時間の読み取り専用鍵で SigV4 署名 |
| 秘密情報 | サーバが全バケットの鍵（暗号化）と STS 用 IAM キーを保持 | 中央には無い。鍵の元（Cloudflare API トークン / OIDC ロール）は各 Pack リポジトリの Actions にだけある |
| 一覧 | バケットのライブ一覧 | マニフェストに明示的に公開したものだけ（SHA-256 固定、不変） |
| 運用 | Workers + D1 または Lambda + DynamoDB、WAF、Terraform | なし（各 Pack リポジトリの scheduled workflow のみ） |
| 監査 | サーバの監査ログ | GitHub の権限変更履歴とストレージのアクセスログ（鍵 ID 単位） |
