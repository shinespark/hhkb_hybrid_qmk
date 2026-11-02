# hhkb_hybrid_qmk

HHKB Professional Hybrid（英語配列）向けの QMK をビルドし、macOS から書き込む非公式の CLI です。純正ファームウェアへ戻すこともできます。

動作を確認した環境は、macOS と英語配列の 1 台だけです。

## 免責事項

⚠️ 利用する前に確認してください。

- 本プロジェクトは PFU Limited および HHKB とは無関係の非公式なものです。本プロジェクトについて PFU に問い合わせないでください。記載の製品名は各社の商標または登録商標です
- 内容は無保証です。キーボードの故障、データの消失、その他の損害を含め、利用は自己責任です。純正以外のファームウェアを書き込むと、メーカーの保証を受けられなくなる可能性があります
- 本リポジトリは、公式ツール、公式ファームウェア、そこから抽出したデータを含まず、配布しません。公式ツールとファームウェアの利用には、PFU の使用許諾と法令が適用されます。参考実装 [l3tnun/hhkb_hybrid](https://github.com/l3tnun/hhkb_hybrid) は submodule で参照するだけです。取得して使うかは利用者自身で判断してください

## 注意事項

- 英語配列以外の機種は未検証です
- 公式ツールが通知するファームウェア更新は適用しないでください。`hhkb flash` は純正 `A0.48` を前提としています
- QMK の動作中は公式ツールを起動しないでください。送信内容が未確認で、純正を前提とする書き込みが発生する可能性があります
- ファームウェアの書き込み中は、USB ケーブルを抜かず、PC をスリープさせないでください
- ビルドした `.bin` と生成した `.hfb` は公開も配布もしないでください

## 必要なもの

- macOS と Rust のツールチェーン
- 純正の更新ファイル `local/HHKB800_FW_A048.hfb`。本リポジトリには含まないので、利用者自身で用意してください
- Docker Desktop
- もう 1 枚のキーボード
- macOS から戻せないときに使う [Linux の環境](docs/analysis/findings.md#linux-からの復旧手順)

## QMK の適用

コマンドはリポジトリのルートで実行します。

### 1. ビルドの準備

```sh
git submodule update --init third_party/hhkb_hybrid
git -C third_party/hhkb_hybrid submodule update --init --depth 1 qmk_firmware
git -C third_party/hhkb_hybrid/qmk_firmware submodule update --init --depth 1 lib/chibios lib/chibios-contrib lib/lufa lib/printf
```

### 2. ビルドと HFB の生成

Docker Desktop を起動してから実行します。`docker` コマンドが見つからないときは、Docker Desktop の CLI を PATH に追加してください。HHKB には接続しません。

```sh
cargo run -p hhkb -- qmk build default
cargo run -p hhkb -- qmk hfb default --bin local/qmk-build/default-arm64/hhkb_hybrid_default.bin --stock local/HHKB800_FW_A048.hfb --out local/hhkb_hybrid_default.hfb
```

### 3. 書き込む前の確認

- 公式ツールで現在の設定を `.hks` に保存する
- [Linux からの復旧手順](docs/analysis/findings.md#linux-からの復旧手順)を実行できる環境を用意する
- 公式ツールと VIA のタブを閉じる
- USB に接続する HHKB を 1 台にし、ハブを使わず直結する
- HHKB とペアリングした機器の Bluetooth を切る。Bluetooth 接続時に転送が止まる例が参考実装の資料にあります
- 別のキーボードを接続する。更新モードでは HHKB から入力できない可能性があります

### 4. 書き込み

1. HFB の種類と接続状況を確認します。HHKB には送信しません

   ```sh
   cargo run -p hhkb -- flash plan local/hhkb_hybrid_default.hfb
   ```

2. 更新モードへ移行します。確認文は `enter update mode for qmk default` です

   ```sh
   cargo run -p hhkb -- flash enter local/hhkb_hybrid_default.hfb
   ```

3. HHKB からキー入力できないことを確認します。USB の表示では純正と更新モードを区別できません。キー入力できるときは次へ進まないでください

4. 本体を書き込みます。確認文は `write qmk default` で、別のキーボードで入力します

   ```sh
   caffeinate -i cargo run -p hhkb -- flash write local/hhkb_hybrid_default.hfb
   ```

5. USB の表示とキー入力を確認します

## VIA の適用

[QMK の適用](#qmk-の適用)と同じ手順で、`default` を `via` に置き換えます。ビルドの準備と書き込む前の確認は共通です。

```sh
cargo run -p hhkb -- qmk build via
cargo run -p hhkb -- qmk hfb via --bin local/qmk-build/via-arm64/hhkb_hybrid_via.bin --stock local/HHKB800_FW_A048.hfb --out local/hhkb_hybrid_via.hfb
cargo run -p hhkb -- flash plan local/hhkb_hybrid_via.hfb
cargo run -p hhkb -- flash enter local/hhkb_hybrid_via.hfb
caffeinate -i cargo run -p hhkb -- flash write local/hhkb_hybrid_via.hfb
```

確認文は、`flash enter` が `enter update mode for qmk via`、`flash write` が `write qmk via` です。

- VIA で接続エラーが出たときは、ページを再読み込みしてください
- Bluetooth で接続できないときは、接続先を登録し直してください。Mac の Bluetooth の一覧に、同じ名前の古い項目が残ることがあります

## 純正へ戻す

[書き込む前の確認](#3-書き込む前の確認)の後に実行します。キー入力できないことを確認してから `flash write` を実行してください。

```sh
cargo run -p hhkb -- flash plan local/HHKB800_FW_A048.hfb
cargo run -p hhkb -- flash enter local/HHKB800_FW_A048.hfb
caffeinate -i cargo run -p hhkb -- flash write local/HHKB800_FW_A048.hfb
```

確認文は、`flash enter` が `enter update mode for stock`、`flash write` が `write stock firmware` です。

`via` から戻すと VIA の設定が消え、純正のキーマップが既定の配置に変わることがあります。`via` を書き込む前に `.hks` を保存し、戻した後に公式ツールで復元してください。

## 書き込みを中止したとき

CLI が到達した段階と次の手順を表示します。HHKB の状態は、キー入力の可否で判断してください。

1. 表示された案内に従い、`hhkb flash` のやり直しを判断します
2. macOS から戻せないときは、Linux から復旧します
3. 最終手段は分解です

QMK の動作中に中止したときは、Esc を押しながら USB を接続して更新モードへ入る手順も表示します。この手順は未検証です。

## コマンド

`cargo run -p hhkb -- <コマンド>` で実行します。`<keymap>` は `default` または `via` です。

| コマンド | 内容 | HHKB への送信 |
|---|---|---|
| `list` | USB の情報と推定した状態を表示する | なし |
| `status` | 状態と応答の概要を表示する | 純正または更新モードに見えるときだけ `02` |
| `inspect <HFB>` | HFB の領域、整合性、機種、バージョンを表示する | なし |
| `flash plan <HFB>` | HFB の種類、パケット数、接続状況を表示する | なし |
| `flash enter <HFB>` | 更新モードへ移行する | 移行のレポート |
| `flash write <HFB>` | 更新モードの HHKB に本体を書き込む | 本体 |
| `qmk build <keymap>` | QMK をビルドし、`.bin` の SHA-256 を照合する | なし |
| `qmk hfb <keymap>` | 純正 HFB と `.bin` から HFB を生成する | なし |

### 制限

- `hhkb flash` で書き込めるのは、SHA-256 が許可リストと一致する純正、`default`、`via` の HFB だけです
- 確認を省略するオプションはありません。再送や自動のやり直しもしません
- 純正の動作中は、HHKB と HFB のバージョンが一致しないと更新モードへ移行しません
- QMK の動作中や状態を確定できないときは、`flash write` は何も送信しません
- `qmk build` は、参考実装と QMK 本体のコミットを検証してからビルドします。`.bin` の SHA-256 が期待値と異なるときは 0 以外で終了します。一致はビルド結果が同じであることを示すもので、動作を保証するものではありません
- `qmk hfb` のテンプレートには、許可した純正 HFB だけを指定できます。出力は `local/` に権限 `0600` で新規作成し、既存のファイルは上書きしません
- ログは `local/logs/` に権限 `0600` で作成します。応答にシリアル番号が含まれるので共有しないでください

## ライセンス

- 参考実装のファイルは MIT、GPL-2.0-or-later、Apache-2.0 が混在します。上流の QMK 本体などは、それぞれのライセンスに従います。正確な内容は参考実装の `LICENSE`、`README.md`、各ファイルの `SPDX-License-Identifier` を確認してください
- `.bin` は QMK 本体（GPL-2.0）と結合したものです。`.hfb` には PFU 製の nRF イメージが含まれます
- これらのライセンスは、PFU の公式ファームウェアや公式ツールと、そこから派生したものについて権利を与えません

## 未検証の項目

- 書き込みが途中で止まった状態からの復旧
- Esc を押しながら USB を接続して更新モードへ入る手順
- 電源を切った後の設定の保持
- 読み出しに現れないファームウェア内部の状態
- 他の個体と他の機種での動作

## ドキュメント

- [解析記録](docs/analysis/findings.md)
- [通信の記録](docs/analysis/traffic-capture.md)
