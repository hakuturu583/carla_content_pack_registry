# carla-pack

CARLA 用 Content Pack を **GitHub のリポジトリ権限で配布する** ためのクライアントです。サーバはありません。

```
carla-pack ──(1) GitHub API: carla-packs.toml を読む ───────────► GitHub（Private リポジトリ）
           ──(2) GitHub API: 読み取り専用の S3 鍵を読む ─────────►   └ Release "carla-pack-credentials"
           ──(3) SigV4 で直接ダウンロード ───────────────────────► S3 互換ストレージ（R2 / S3 / MinIO …）
           ──(4) SHA-256 と carla-pack.json で検証 → 配置 → mount ─► CARLA サーバ
```

- **アクセス権の確認は GitHub が行います。** Pack リポジトリ（Private）を読めるユーザーだけが、
  マニフェスト `carla-packs.toml` と、Release に置かれた読み取り専用のストレージ鍵を取得できます。読めない人には GitHub が 404 を返すだけです。
  誰に配るかは、そのリポジトリの Read 権限（ユーザー / Team / Organization）でそのまま決まります。
- **ファイルは S3 互換ストレージから直接** ダウンロードします（Cloudflare R2、AWS S3、MinIO、SeaweedFS など）。
  誰に配ってもよい Pack は **Hugging Face Hub の公開リポジトリ** にも置けます（鍵不要、匿名で取得）。
- 鍵は Pack リポジトリの GitHub Actions が **6 時間ごとに作り直す有効期限 12 時間の一時鍵** で、バケットの Pack 用プレフィックスを読むことしかできません。
  R2 は一時認証情報 API、AWS は GitHub OIDC で発行するので、長期の鍵はどこにも保存しません。
- ダウンロードしたファイルはマニフェストの **SHA-256 とサイズ**、さらに Pack 自身の `carla-pack.json` の全ファイルのハッシュで検証してから配置します。
- コア（GitHub API、SigV4、ダウンロード、検証、展開）は Rust、CLI と CARLA へのマウントは Python です（PyO3 / maturin、abi3 wheel）。

旧版（Cloudflare Workers / AWS Lambda の Registry サーバ）は廃止しました。その設計との違いは [docs/architecture.md](docs/architecture.md) にあります。

## 使う（Pack をダウンロードする側）

```bash
pip install carla-pack            # 公開後。開発中は「開発」を参照

gh auth login                     # GitHub CLI のトークンをそのまま使います（GH_TOKEN / GITHUB_TOKEN / carla-pack login でも可）
carla-pack whoami
carla-pack repo add tier4/carla-packs     # マップ名で探すときの検索対象（CARLA_PACK_REPOS でも指定可）

carla-pack ls                             # 読めるリポジトリの Pack 一覧
carla-pack find NishishinjukuMap          # そのマップを含む Pack
carla-pack install NishishinjukuMap --map \
  --packs-dir /opt/carla/Linux/CarlaUnreal/Packs --mount localhost:2000
carla-pack install tier4/carla-packs/NishishinjukuPack@1.2.0 --packs-dir ./Packs   # 参照で指定
carla-pack pull tier4/carla-packs/NishishinjukuPack -o ./downloads               # tarball だけ取得
```

Pack の指定は `OWNER/REPO/PACK[@VERSION]` です。バージョンを省略すると最新（`--base-release` / `--platform` で絞り込み）、
`--sha256` を付けるとその成果物に固定します（lock ファイル用）。

### Python から（シナリオライブラリへの組み込み）

```python
import typesafe_carla.carla as carla   # 公式の carla モジュールでも可
from carla_pack_registry import Client, deploy_pack, ensure_map

client = carla.Client("localhost", 2000)

# サーバに無いマップだけ、読めるリポジトリから探して取得・検証・配置・mount する。あれば何もしない（None）。
pack = ensure_map(client, "NishishinjukuMap", "/opt/carla/Linux/CarlaUnreal/Packs",
                  repos=["tier4/carla-packs"])
client.load_world("NishishinjukuMap")

# lock ファイルの内容（参照 + SHA-256）で再現性のある取得
deploy_pack("tier4/carla-packs/NishishinjukuPack", packs_dir, carla_client=client,
            sha256=pack["sha256"])

# 低レベル API
c = Client()                                     # トークン・検索対象は環境 / 保存済み設定から
release = c.find_map("NishishinjukuMap")         # {"ref", "repo", "name", "version", "sha256", "maps", ...}
path = c.fetch(release)                          # キャッシュ（SHA-256 名）に取得して検証、パスを返す
c.install(release, packs_dir)                    # 検証して <packs_dir>/<name> に配置（同一なら何もしない）
```

- `packs_dir` は CARLA サーバから同じパスで読める場所にしてください（パッケージの `Linux/CarlaUnreal/Packs`、`-carla-packs=<dir>`、または mount するなら任意のフォルダ）。
- 同じ Pack（名前・バージョン・base release・platform）がインストール済みならダウンロードしません。別バージョンの置き換えは `force=True`（mount 中なら先に unmount）。
- ダウンロードは `~/.cache/carla-pack/blobs/<sha256>.tar.gz` にキャッシュします（`CARLA_PACK_CACHE_DIR`）。
  数 GB の Pack は、キャッシュの tarball と展開後の Pack で **サイズの約 2 倍** のディスクを使います。
- 接続が途中で切れても、受け取った位置から Range で再開します（最大 5 回）。再開しても全体の SHA-256 で検証します。

### 地図データを形式で引く

Pack が持つ地図データを、マップ名と形式名でローカルのパスとして取り出せます。CARLA サーバは要りません（Pack はキャッシュに取得・検証・展開されます）。

- どの Pack でも、`carla-pack.json` にある標準の形式（`opendrive` / `navmesh` / `traffic_manager` / `map_logic`）。
- Pack が `Content/Config/roadgen_asset.json`（任意）を持っていれば、その形式も（roadgen の `carla_pack_assets` が書く: `lanelet2` / `autoware`（Lanelet2・点群地図・projector）/ `pointcloud` / `sumo` / `commonroad` / `clipgt` / `rdshq` / `gpudrive` / `womd` など）。

```bash
carla-pack formats hakuturu583/carla-packs-test/RoadgenHighwayPack           # マップ × 形式の一覧
carla-pack data hakuturu583/carla-packs-test/RoadgenHighwayPack --format lanelet2
carla-pack data RoadgenHighway --map --format autoware --file pointcloud_map  # フォルダの中の 1 ファイル
carla-pack find RoadgenHighway --format lanelet2      # その形式を持つ Pack（公開時の索引で探す）
carla-pack find SomeDataOnlyMap --assets              # asset ファイルにだけあるマップも探す
carla-pack ls --formats                               # 一覧に形式の列を出す
carla-pack formats ./Packs/RoadgenHighwayPack         # インストール済みの Pack フォルダや tarball も可
```

```python
from carla_pack_registry import Client, map_data, map_formats

client = Client()
release = client.find_map("RoadgenHighway", format="autoware")
autoware_dir = client.map_data(release, "RoadgenHighway", "autoware")   # Autoware の map_path にそのまま
pcd = client.map_data(release, "RoadgenHighway", "autoware", file="pointcloud_map")
client.formats(release)          # {map: {format: {type, path, files, origin}}}
map_formats("/opt/carla/CarlaUnreal/Packs/RoadgenHighwayPack")          # インストール済みの Pack
map_data("/opt/carla/CarlaUnreal/Packs/RoadgenHighwayPack", "RoadgenHighway", "opendrive")
```

`carla-pack publish` は、マップごとの形式を `carla-packs.toml` のエントリ（`formats`）に書きます。`find --format` はこの索引で探すので、ダウンロードしません。

### 設定

| 項目 | 方法 |
| --- | --- |
| GitHub トークン | `CARLA_PACK_TOKEN` → `GH_TOKEN` → `GITHUB_TOKEN` → `carla-pack login` で保存したもの → `gh auth token` の順 |
| 検索対象のリポジトリ | `--repo`（繰り返し可）、`CARLA_PACK_REPOS`（カンマ区切り）、`carla-pack repo add` |
| 保存場所 | `CARLA_PACK_CONFIG_DIR`（既定 `~/.config/carla-pack/config.json`、パーミッション 600） |
| GitHub Enterprise Server | `CARLA_PACK_GITHUB_API`（例 `https://ghe.example.com/api/v3`） |

`carla-pack login` は GitHub App（推奨: Contents 読み取り専用、Pack リポジトリにインストール）の client ID によるデバイスフローです
（`--client-id` / `CARLA_PACK_GITHUB_CLIENT_ID`、OAuth App なら `--scope repo`）。`gh auth login` 済みなら不要です。
CI では Pack リポジトリを読める fine-grained PAT か GitHub App のトークンを `CARLA_PACK_TOKEN` に渡してください
（ワークフローの `GITHUB_TOKEN` は他のリポジトリを読めません）。

## 配る（Pack リポジトリを作る側）

[docs/pack-repository.md](docs/pack-repository.md) を参照してください。概要:

1. Private リポジトリを作り、[templates/pack-repo/carla-packs.toml](templates/pack-repo/carla-packs.toml) を置く。配りたい相手に Read 権限を付ける。
2. バケット（R2 / S3）と読み取り専用の鍵の発行元を用意し、[鍵のローテーション workflow](templates/pack-repo/.github/workflows/) をコピーする。
3. `carla-pack publish <Pack>.tar.gz` でアップロードとマニフェストへの追記を行い、commit / push する。
   アップロード先は R2 / S3（carla-pack が直接マルチパートで送る）か Hugging Face（`pip install 'carla-pack[huggingface]'`、`huggingface_hub` 経由）。

## 開発

```bash
uv venv && . .venv/bin/activate
uv pip install "maturin>=1.9,<2" pytest "moto[server]==5.2.5" boto3 "huggingface_hub>=0.32"
maturin develop            # Rust コアをビルドして carla_pack_registry をインストール
cargo fmt --check && cargo clippy --all-targets --features python -- -D warnings
cargo test                 # Rust: SigV4 の公式テストベクタ、GitHub / S3 のモックサーバでの全フロー、展開の安全性
pytest                     # Python: CARLA 側（mount / unmount の順序）と、moto（IAM 認証付き S3）+ 偽 GitHub での E2E
maturin build --release    # wheel（abi3: Python 3.10 以上で共通）
```

## ドキュメント

| ドキュメント | 内容 |
| --- | --- |
| [docs/architecture.md](docs/architecture.md) | 仕組み、マニフェストと鍵ファイルの形式、旧 Registry サーバとの違い |
| [docs/security.md](docs/security.md) | 信頼の境界、失効、残留リスク |
| [docs/pack-repository.md](docs/pack-repository.md) | Pack リポジトリの作り方（R2 / AWS / その他の S3 互換） |

## ライセンス

[Apache License 2.0](LICENSE) です。
