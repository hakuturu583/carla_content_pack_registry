# carla-pack

CARLA Content Pack Registry のコマンドラインクライアントです。

- コア（API 通信、GitHub ログイン、トークン保存、ダウンロード）は Rust、コマンドラインは Python です（PyO3 / maturin）。
- ファイルは Registry を経由せず、R2 / S3 から署名付き URL で直接ダウンロードします。

## インストール

```bash
pip install carla-pack-registry   # 開発中は下の「開発」を参照
```

PyPI の配布名とモジュール名は `carla-pack-registry` / `carla_pack_registry`、コマンド名は `carla-pack` です（CARLA 本体の `Util/ContentPacks/carla_pack.py` と同じ環境で import しても衝突しません）。

## 使い方

```bash
carla-pack login                         # GitHub でログイン（表示されたコードをブラウザで入力）
carla-pack whoami

carla-pack ls                            # 読めるバケットの一覧
carla-pack ls alice/maps                 # バケット内のファイル一覧
carla-pack ls alice/maps Town10/         # プレフィックスで絞り込み
carla-pack pull alice/maps maps/Town10HD_Opt.tar.zst -o ./packs
carla-pack pull alice/maps maps/          # プレフィックス配下をすべて
carla-pack pull alice/maps                # バケット全体（-j で並列数、既定 4）
```

バケットは `OWNER/NAME`、`NAME`（一意な場合）、またはバケット ID で指定します。

### Content Pack をインストールして CARLA に読み込む

CARLA の `Util/ContentPacks/carla_pack.py build` が作る pack の tarball（`<Pack>-<version>-<base release>.tar.gz`）を、ダウンロード → 検証 → 配置 → マウントまで一度に行います。

```bash
carla-pack install alice/maps packs/carla-0.10.0-Linux/Town01Pack-1.0.0-carla-0.10.0-Linux.tar.gz \
  --packs-dir /opt/carla/Linux/CarlaUnreal/Packs \
  --base-release carla-0.10.0-Linux \
  --mount localhost:2000                 # 省略するとインストールのみ（次回起動時に読み込まれる）
```

```python
import carla
from carla_pack_registry import deploy_pack

client = carla.Client("localhost", 2000)
pack = deploy_pack("alice/maps", "packs/carla-0.10.0-Linux/Town01Pack-1.0.0-carla-0.10.0-Linux.tar.gz",
                   "/opt/carla/Linux/CarlaUnreal/Packs", carla_client=client, base_release="carla-0.10.0-Linux")
client.load_world(pack["maps"][0])
```

- tarball は Registry を経由せず、ストレージから直接ダウンロードします。展開は単一のトップフォルダの通常ファイル・ディレクトリだけを受け付け（`..`・絶対パス・リンクは拒否）、`carla-pack.json` の SHA-256 と全ファイルを照合し、manifest にないファイルがあれば拒否します。失敗したときは `--packs-dir` に何も残しません。
- `--packs-dir` は CARLA サーバから同じパスで読める場所にしてください（パッケージの `Linux/CarlaUnreal/Packs`、`-carla-packs=<dir>` で指定したディレクトリ、またはマウントするなら任意のフォルダ）。
- `--base-release` を付けると、別リリース向けの pack をインストール前に拒否します。マウント時にはサーバ側でも照合されます。
- `-f` / `force=True` は同名の pack を置き換え、マウント中なら置き換える前にアンマウントします。そのマップを読み込み中などでサーバがアンマウントを拒否した場合は、何もインストールしません（別のマップを読み込むか、サーバを再起動してください）。
- `--mount` には CARLA の Python API（パッケージの `PythonAPI/carla/dist` の wheel）が必要です。

### 自分のバケットを登録する

```bash
# Cloudflare R2（読み取り専用・バケット限定の API トークンを使う）
export CARLA_PACK_SECRET_ACCESS_KEY=...   # シークレットは引数では受け付けません（未設定なら入力を求めます）
carla-pack bucket add my-maps --provider r2 --account-id <32桁> --bucket <bucket> --access-key-id <id>

# AWS S3（IAM ロール。表示される principal と externalId を信頼ポリシーに設定）
carla-pack bucket add my-maps --provider s3 --region ap-northeast-1 --bucket <bucket> \
  --role-arn arn:aws:iam::<account>:role/<role>

carla-pack bucket verify alice/my-maps    # 接続確認。Private なのに公開されていると失敗します
carla-pack acl add alice/my-maps --org carla-sim --team carla-sim/maps --user bob
carla-pack acl show alice/my-maps
carla-pack bucket set alice/my-maps --public
carla-pack bucket rm alice/my-maps        # 登録解除のみ。ストレージ上のファイルは消えません
```

ファイルのアップロード（`push`）はまだありません。自分のバケットへは `aws s3 cp` / `wrangler r2 object put` / rclone などで置いてください。

### 設定

| 項目 | 方法 |
| --- | --- |
| Registry | `--registry URL`、`CARLA_PACK_REGISTRY`、または最後にログインした Registry（未設定時は `http://localhost:8787`） |
| トークン | `carla-pack login` で保存。`CARLA_PACK_TOKEN` で上書き可能（CI 向け） |
| 保存場所 | `CARLA_PACK_CONFIG_DIR`、既定は OS の設定ディレクトリ（例: `~/.config/carla-pack/credentials.json`、パーミッション 600） |
| 出力 | `--json` で機械可読な出力 |

ローカル開発（Mock 認証の Registry）では `carla-pack login --token mock:alice` でログインできます。

## 開発

```bash
cd cli
uv venv && . .venv/bin/activate
uv pip install maturin pytest
maturin develop            # Rust コアをビルドして carla_pack_registry をインストール
cargo test                 # Rust のテスト（GitHub / Registry はモックサーバー）
pytest                     # CLI の E2E テスト（ローカル Registry が必要。README ルート参照）
maturin build --release    # wheel を作成（abi3: Python 3.10 以上で共通）
```
