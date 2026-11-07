# Pack リポジトリの作り方

Pack リポジトリは「誰がどの Pack を取得できるか」を表す GitHub リポジトリです。
Read 権限を持つ人だけが中身（Pack の一覧と、ストレージを読むための一時鍵）を取得できます。

共有範囲ごとにリポジトリを分けます（例: 社内全員向け `tier4/carla-packs`、特定 Team 向け `tier4/carla-packs-restricted`）。
ストレージは同じバケットでも構いませんが、**プレフィックスは分けてください**（鍵はプレフィックス単位で発行されます）。

## 1. リポジトリ

1. **Private** リポジトリを作る（公開リポジトリだと鍵の workflow は失敗します）。
2. ルートに [templates/pack-repo/carla-packs.toml](../templates/pack-repo/carla-packs.toml) をコピーし、`[storage]` を埋める。
3. 配りたいユーザー / Team / Organization に **Read** を付ける。**Write** は Pack を公開する人だけにする。
4. 推奨: デフォルトブランチを保護し、`carla-packs.toml` の変更をレビュー必須にする（CODEOWNERS）。

## 2. ストレージと鍵

### Cloudflare R2

1. バケットを作る（公開アクセス・r2.dev・カスタムドメインは **無効**）。
2. **R2 API トークン（親）** を作る: 対象バケットの Object Read。表示される Access Key ID を控える（Secret は不要）。
   一時鍵はこの親から作られ、親の権限を超えません。
3. **Cloudflare API トークン** を作る（R2 の一時認証情報を作成できる権限）。
4. リポジトリの Settings → Secrets and variables → Actions:
   - secret `CLOUDFLARE_API_TOKEN`
   - variables `R2_ACCOUNT_ID`、`R2_BUCKET`、`R2_PREFIX`（例 `packs/`）、`R2_PARENT_ACCESS_KEY_ID`
5. [carla-pack-credentials-r2.yml](../templates/pack-repo/.github/workflows/carla-pack-credentials-r2.yml) を `.github/workflows/` にコピーし、Actions タブから一度手動実行する。

`carla-packs.toml`:

```toml
[storage]
endpoint = "https://<ACCOUNT_ID>.r2.cloudflarestorage.com"
region = "auto"
bucket = "<bucket>"
prefix = "packs/"
credentials = "github-release"
```

### AWS S3

1. バケットを作る（Block Public Access 有効）。
2. GitHub の OIDC プロバイダ（`token.actions.githubusercontent.com`）を IAM に登録する（未登録なら）。
3. IAM ロールを作る:
   - 信頼ポリシー: `token.actions.githubusercontent.com` の `sts:AssumeRoleWithWebIdentity`、
     条件 `token.actions.githubusercontent.com:sub` = `repo:<OWNER>/<REPO>:ref:refs/heads/<デフォルトブランチ>`、`aud` = `sts.amazonaws.com`
   - 権限: 対象バケットの `s3:GetObject`
   - 最大セッション時間: 12 時間
4. variables `AWS_ROLE_ARN`、`AWS_REGION`、`S3_BUCKET`、`S3_PREFIX`。
5. [carla-pack-credentials-aws.yml](../templates/pack-repo/.github/workflows/carla-pack-credentials-aws.yml) をコピーして手動実行。
   セッションはさらにセッションポリシーで `s3:GetObject` と prefix に絞られます。

`carla-packs.toml`:

```toml
[storage]
endpoint = "https://s3.<region>.amazonaws.com"
region = "<region>"
bucket = "<bucket>"
prefix = "packs/"
credentials = "github-release"
```

### その他の S3 互換ストレージ（MinIO、SeaweedFS など）

一時鍵を発行する API が使えない場合は、**読み取り専用・バケット限定の鍵** を手で置きます（期限なし。人を外したら鍵を作り直してください）。

```bash
cat > credentials.json <<'EOF'
{"version": 1, "access_key_id": "...", "secret_access_key": "..."}
EOF
gh release create carla-pack-credentials --prerelease --title "carla-pack storage credentials" --notes "Do not edit." || true
gh release upload carla-pack-credentials credentials.json --clobber
rm credentials.json
```

### Hugging Face（公開 Pack 向け）

誰に配ってもよい Pack は Hugging Face Hub の公開リポジトリ（dataset 推奨）に置けます。
ストレージも鍵も鍵の workflow も不要で、Autoware のマップ（`AutowareFoundation/carla-ue5-maps`）と同じ場所に並べられます。
ダウンロードは匿名の `resolve` URL で、トークンは送りません。中身はマニフェストの SHA-256 で検証します。

1. Hugging Face に公開リポジトリを作る（gated にはしない）。
2. [templates/pack-repo/carla-packs.huggingface.toml](../templates/pack-repo/carla-packs.huggingface.toml) を `carla-packs.toml` としてコピーする。GitHub 側の Pack リポジトリも公開でよい。
3. `carla-pack publish` でアップロードとマニフェストへの追記を行う。Hub へのアップロードは Hugging Face 公式の `huggingface_hub` が行います（大きなファイルも Hub の転送方式でそのまま送れます）:

```bash
pip install 'carla-pack-registry[huggingface]'
hf auth login            # または HF_TOKEN（そのリポジトリへの書き込み権限を持つトークン）
carla-pack publish NishishinjukuPack-1.2.0-carla-0.10.0-Linux.tar.gz
git commit -am "NishishinjukuPack 1.2.0" && git push
```

   アップロード前にすべて確認します（同じリリースの重複、同じパスの既存ファイル。後者は `--overwrite` で置き換え）。
   アップロードに失敗した場合、マニフェストには何も追記しません。`revision` はブランチにしてください（コミットには書き込めません）。

```toml
[storage]
type = "huggingface"
repo = "AutowareFoundation/carla-packs"
repo_type = "dataset"     # 既定。model も可
revision = "main"         # 既定。ブランチ・タグ・コミット
prefix = "packs/"
```

非公開の Pack には使えません（匿名で読むため）。非公開は R2 / S3 に置いてください。

### 公開バケット

誰でも取得してよい Pack は、公開読み取りのバケットに置いて `credentials = "anonymous"` にします（鍵は不要、リポジトリも公開で可）。

## 3. Pack を公開する

CARLA の `Util/ContentPacks/carla_pack.py build` で作った tarball を、書き込み用の鍵でアップロードしてマニフェストに追記します。

```bash
git clone git@github.com:tier4/carla-packs.git && cd carla-packs
export CARLA_PACK_UPLOAD_ACCESS_KEY_ID=...        # 書き込みできる鍵（AWS_ACCESS_KEY_ID などでも可）
export CARLA_PACK_UPLOAD_SECRET_ACCESS_KEY=...
carla-pack publish ~/build/NishishinjukuPack-1.2.0-carla-0.10.0-Linux.tar.gz
git commit -am "NishishinjukuPack 1.2.0" && git push            # （ブランチ保護があるなら PR）
```

- `carla-pack.json` から名前・バージョン・base release・platform・マップを読み、SHA-256 とサイズを計算して `[[pack]]` を追記します（既存のコメントは残ります）。
- 同じ `(name, version, base_release, platform)` がすでにあれば拒否します。**公開済みのリリースは変更しない**でください（中身を変えるならバージョンを上げる）。
- 64 MiB を超えるファイルはマルチパートアップロード（既定 64 MiB ごと、`--part-size-mib` で変更）で送ります。数 GB〜数百 GB の Pack もそのまま公開できます（上限は S3 / R2 の 10,000 パートで、パートサイズは自動で引き上げます）。各パートは失敗すると再試行し、それでも失敗したらアップロードを中止（abort）してマニフェストには何も追記しません。
- 他のツール（`aws s3 cp` / `rclone` など）でアップロード済みなら `--no-upload` で追記だけできます。
- 取り下げは該当エントリに `yanked = true` を追加します（SHA-256 で固定した lock からは引き続き取得できます）。

## 4. 確認

```bash
carla-pack credentials tier4/carla-packs     # 鍵の ID と期限（秘密は表示しません）
carla-pack ls tier4/carla-packs
carla-pack pull tier4/carla-packs/NishishinjukuPack -o /tmp/check
```
