# リリース（PyPI）

Python パッケージ `carla-pack-registry`（コマンド `carla-pack`、モジュール `carla_pack_registry`）は、PR のラベルでリリースします（[simple_lanelet2](https://github.com/hakuturu583/simple_lanelet2) と同じ仕組み、`.github/workflows/release.yml`）。

| ラベル | main にマージしたとき |
| --- | --- |
| `release:patch` | 0.2.0 → 0.2.1 |
| `release:minor` | 0.2.0 → 0.3.0 |
| `release:major` | 0.2.0 → 1.0.0 |
| なし | 何もしない |

ラベルは 1 つだけ付けます（複数あるとリリースジョブが失敗します）。マージされると同じワークフロー実行の中で次が順に走ります。

1. `scripts/bump-cli-version.py` で `Cargo.toml` と `Cargo.lock` のバージョンを上げ、`chore(release): vX.Y.Z` を main にコミット、`vX.Y.Z` タグと GitHub Release（自動生成ノート）を作成
2. タグのコミットで fmt / clippy / `cargo test` を再実行し、タグとパッケージのバージョンの一致を確認
3. Linux x86_64 / aarch64（manylinux 2_28）、macOS arm64 / x86_64、Windows x64 の wheel（abi3、Python 3.10 以上）を各アーキテクチャのランナーでビルドし、クリーンな venv にインストールしてテスト。sdist はビルドしてから一度インストールして確認
4. `twine check --strict` の後、PyPI に Trusted Publishing でアップロード（`skip-existing`。失敗した分は再実行で追加できます）

wheel のテストは extra なしで行うので、moto を使う E2E と Hugging Face のテストはスキップされます。これらは `ci.yml` が PR ごとに実行しています。

バージョンの唯一の定義は `Cargo.toml` の `[package] version` です（`pyproject.toml` は dynamic）。手で変えず、ラベルか `scripts/bump-cli-version.py` を使ってください。

## 初回だけ必要な設定

1. PyPI（https://pypi.org/manage/account/publishing/）で **pending publisher** を追加します: PyPI project name `carla-pack-registry`、owner `hakuturu583`、repository `carla_content_pack_registry`、workflow `release.yml`、environment `pypi`。
2. GitHub の Settings → Environments に `pypi` を作ります（必要なら Required reviewers を付けると、公開前に承認を挟めます）。

設定前に release ラベルの PR をマージすると、タグと GitHub Release までは作られ、publish ジョブだけが失敗します。wheel と sdist はその実行の artifact に残るので、設定後に「Re-run failed jobs」で公開できます。

## その他の入口

- **試しにビルドする**: Actions → Release → Run workflow（`workflow_dispatch`）。wheel と sdist を作ってテストするだけで、タグも公開もしません。`release.yml`、`scripts/bump-cli-version.py`、`Cargo.*`、`pyproject.toml`、`README.md`、`LICENSE` を変更する PR でも同じドライランが走ります。
- **手でタグを切る（hotfix など）**: バージョンを `scripts/bump-cli-version.py X.Y.Z` で上げてコミットし、`git tag -a vX.Y.Z && git push origin vX.Y.Z`。タグのプッシュでビルドと公開だけが走ります（タグとパッケージのバージョンが違うと止まります）。
