# hhkb_hybrid_qmk

HHKB Professional Hybrid（英語配列）を QMK 化して macOS から書き込むための、個人的な調査とツールです。現在は検証の段階です。純正の更新ファイルを macOS から書き込む CLI があり、手元の 1 台で 1 回成功しています。QMK はビルドして HFB を生成でき、CLI で書き込めるようにしましたが、実機での動作は未確認です。

## 免責事項

- 本プロジェクトは PFU Limited および HHKB とは無関係の非公式なものです。PFU による承認、保証、サポートはありません
- 本プロジェクトの内容は無保証です。利用は自己責任で行ってください。キーボードの故障、データの消失、その他の損害について、作者は責任を負いません
- 純正以外のファームウェアを書き込むと、メーカーの保証を受けられなくなる可能性があります。本プロジェクトに関することを PFU に問い合わせないでください
- 公式ツールやファームウェアの利用には、PFU の使用許諾と適用される法令が適用されます。内容を確認し、利用者自身の判断で扱ってください
- 本プロジェクトの記述は法的な助言ではありません。また、特定の行為が適法であることや使用許諾に違反しないことを保証するものでもありません
- 本リポジトリは、公式ツール、公式ファームウェア、そこから抽出したデータを含まず、配布しません
- 参考実装 [l3tnun/hhkb_hybrid](https://github.com/l3tnun/hhkb_hybrid) は submodule でコミットを固定して参照するだけで、ファイルを本リポジトリに含みません。参考実装の定義には、作者が公式ファームウェアの逆アセンブルで得た知見が反映されています。取得して使うかは、利用者自身の判断で決めてください
- 解析記録は手元の 1 台の観測にもとづくものです。他の個体や版に当てはまるとは限りません
- 記載の製品名は各社の商標または登録商標です

## 注意事項

### 全般

- QMK の書き込みと純正への復帰は、実機で未確認です。手順や結論は検証の進行に合わせて変わります
- 確認しているのは英語配列の 1 台だけです。他の機種で試さないでください
- USB に接続する対象のキーボードは 1 台だけにしてください
- 公式ツールが通知するファームウェア更新は、計画に沿って判断するまで適用しないでください。版が変わると検証の前提が崩れます
- 実機を操作する前に、公式ツールで現在の設定を `.hks` に保存してください

### 通信の記録

- ロガーは公式ツールのプロセス内で動きます。記録に使うのは、内容を確認した手順だけにしてください
- 公式ツールは起動しただけで実機と通信します。接続したまま起動すると、操作しなくても通信が発生します
- 記録中は公式ツールを強制終了しないでください
- 安全弁で公式ツールが中止したときは、USB を抜き差しして、キー入力できることを確認してください
- ファームウェア更新の実行中は、USB ケーブルを抜かず、PC をスリープさせないでください。失敗すると、復旧に分解が必要になる可能性があります

### 情報の取り扱い

- 純正 HFB、通信ログ、`.hks` は `local/` に置き、コミットや共有をしないでください
- 通信ログと公式ツールのログ（`~/Library/Application Support/hhkb-keymap-tool/`）にはシリアル番号が含まれる場合があります。内容を貼り付けて共有しないでください
- 公開する前に、ファームウェアのバイト列や個体情報が含まれていないことを確認してください

## macOS からの純正の再書き込み

`hhkb flash` は、SHA-256 を許可した純正 `A0.48` の HFB だけを書き込みます。手元の 1 台で、実機と同じ版を 1 回書き込んで成功しました。結果は[解析記録](docs/analysis/findings.md#macos-からの純正の再書き込み)にあります。他の個体や、途中で止まった場合の動作は確認していません。

### 実行前の注意

- 書き込みは保証に影響する可能性があります。失敗したときは Linux からの復旧が必要で、最終手段は分解です
- 実行する前に、[Linux からの復旧手順](docs/analysis/findings.md#linux-からの復旧手順) を実行できる環境を用意してください。この手順でも、途中で止まった状態から戻せることは未検証です
- 実機の設定を公式ツールで `.hks` に保存してください
- 公式ツールを閉じ、USB に接続する HHKB を 1 台にして、ハブを使わず直結してください
- HHKB とペアリングした機器の Bluetooth を切ってください。参考実装の資料には、Bluetooth 接続時に転送が止まる例があります
- 更新モードでは HHKB から入力できない可能性があるので、別のキーボードを用意してください
- 確認を省略するオプションはありません。再送や自動のやり直しもしません

### 手順

コマンドはリポジトリのルートで実行します。ログは `local/logs/` に権限 `0600` で作成します。本体のデータはパケット番号だけを記録しますが、応答にはシリアル番号が含まれるので共有しないでください。

1. 計画を確認します。実機には何も送信しません

   ```sh
   cargo run -p hhkb -- flash plan local/HHKB800_FW_A048.hfb
   ```

2. 更新モードへ移行します。実機の版が HFB の版と一致したときだけ移行のレポートを送り、再列挙されたインターフェースを開けることを確かめて終了します。本体は送信しません。確認文は `enter update mode for stock` です

   ```sh
   cargo run -p hhkb -- flash enter local/HHKB800_FW_A048.hfb
   ```

3. HHKB からキー入力できないことを確かめてから、本体を書き込みます。USB の表示では純正 FW と更新モードを区別できません。キー入力できるときは実行しないでください。確認文は `write stock firmware` で、別のキーボードで入力します

   ```sh
   caffeinate -i cargo run -p hhkb -- flash write local/HHKB800_FW_A048.hfb
   ```

4. USB の表示、キー入力、`hhkb status`、公式ツールの読み出しを確認します

中止したときは、到達した段階と次の手順を表示します。実機の状態は断定しないので、キー入力の可否で確かめてください。

## QMK のビルドの準備

QMK のキーボード定義は、参考実装 [l3tnun/hhkb_hybrid](https://github.com/l3tnun/hhkb_hybrid) をコミット固定の submodule で参照します。QMK 本体は、参考実装が固定したコミットを取得します。次のコマンドをリポジトリのルートで実行します。実行するのは Git の取得だけです。

```sh
git submodule update --init third_party/hhkb_hybrid
git -C third_party/hhkb_hybrid submodule update --init --depth 1 qmk_firmware
git -C third_party/hhkb_hybrid/qmk_firmware submodule update --init --depth 1 lib/chibios lib/chibios-contrib lib/lufa lib/printf
```

ライセンスと、生成物の扱いは次のとおりです。正確な内容は参考実装の `LICENSE` と `README.md`、各ファイルの `SPDX-License-Identifier` を確認してください。

- 参考実装のファイルは、MIT、GPL-2.0-or-later、Apache-2.0 が混在します。上流の QMK 本体などは、それぞれのライセンスに従います
- ビルドした `.bin` は QMK 本体（GPL-2.0）と結合したものです。公開も配布もしないでください
- 生成した `.hfb` には PFU 製の nRF イメージが含まれます。公開も配布もしないでください
- これらのライセンスは、PFU の公式ファームウェアや公式ツールと、そこから派生したものについて権利を与えません

## QMK のビルドと HFB の生成

実機には触れません。Docker Desktop を起動しておき、前の節の準備の後に、リポジトリのルートで実行します。`docker` コマンドが見つからないときは、Docker Desktop の CLI を PATH に追加してください。

```sh
cargo run -p hhkb -- qmk build default
cargo run -p hhkb -- qmk hfb default --bin local/qmk-build/default-arm64/hhkb_hybrid_default.bin --stock local/HHKB800_FW_A048.hfb --out local/hhkb_hybrid_default.hfb
```

`default` を `via` に置き換えると、VIA 対応版を作れます。

- `qmk build` は、参考実装と QMK 本体のコミットを確かめてからビルドし、`.bin` の SHA-256 を期待する値と照合します。一致しないときは 0 以外で終了します
- `qmk hfb` は、許可した純正 HFB の領域 A だけを `.bin` に置き換えて CRC を計算し直し、`local/` に権限 `0600` で新規作成します。既存のファイルは上書きしません
- 期待する値は、固定したコミットの手元での実測値です。ビルドの結果が同じであることを確かめるもので、ファームウェアが正しく動くことの保証ではありません。実機での動作は未確認です
- `qmk hfb` のテンプレートには純正の HFB だけを指定できます。生成した HFB は指定できません
- `hhkb flash` の許可リストにあるのは、期待する値から生成した `default` と `via` の HFB だけです。入力や内容が異なる HFB は書き込めません

## QMK の書き込みと純正への復帰

`hhkb flash` は、許可した 3 つの HFB（純正、QMK の `default`、QMK の `via`）を書き込めます。**QMK の動作中の判定、移行のレポート、復帰の手順は、参考実装の資料とコードにもとづくもので、実機では未確認です。**

### 実行前の注意

- 「macOS からの純正の再書き込み」の実行前の注意をすべて確認してください
- QMK を書き込むと、メーカーの保証を受けられなくなる可能性があります。戻せない場合は分解が必要になる可能性があります
- 途中で止まった状態から戻せるかは未検証です
- QMK の動作中は公式ツールを起動しないでください。送信の内容が未確認で、純正を前提とする書き込みが起きる可能性があります
- `via` を書き込む前と純正へ戻す前は、VIA のタブを閉じてください
- 純正へ戻すと VIA の設定は消えます。純正の設定が変わっていたときは、公式ツールと `.hks` で戻します

### 手順

手順は純正の再書き込みと同じで、`flash plan`、`flash enter`、キー入力の確認、`flash write` の順に実行します。`flash plan` は HFB の種類と実機の状態を表示します。`hhkb list` と `hhkb status` は、QMK の動作中に見えるときはデバイスを開かず、何も送信しません。

```sh
cargo run -p hhkb -- flash plan local/hhkb_hybrid_default.hfb
cargo run -p hhkb -- flash enter local/hhkb_hybrid_default.hfb
caffeinate -i cargo run -p hhkb -- flash write local/hhkb_hybrid_default.hfb
```

`default` を `via` に置き換えると VIA 対応版を、`local/HHKB800_FW_A048.hfb` にすると純正を書き込みます。確認文は段階と種類ごとに異なります。

| 書き込む HFB | `flash enter` | `flash write` |
|---|---|---|
| 純正 | `enter update mode for stock` | `write stock firmware` |
| QMK の `default` | `enter update mode for qmk default` | `write qmk default` |
| QMK の `via` | `enter update mode for qmk via` | `write qmk via` |

`flash enter` が送るレポートは、実機の状態と HFB の種類で決まります。

| 実機の状態 | 書き込む HFB | 送るレポート |
|---|---|---|
| 純正 FW または更新モード | すべて | `0xFF00` へ 64 バイトで `02` の後に `e0` |
| QMK の動作中 | QMK | `0xFF60` へ 32 バイトで `e0` |
| QMK の動作中 | 純正 | `0xFF60` へ 32 バイトで `f0`。拒否されたら VIA の問い合わせ `01` を送り、応答がないときだけ `e0` |
| 確定できない | すべて | 送らない |

- 純正 FW の動作中は、実機の版が HFB の版と一致したときだけ移行します
- `flash write` は、QMK の動作中や状態を確定できないときは何も送信しません。`0xFF60` が見えている間は更新モードとして扱いません
- QMK の動作中に中止したときは、Esc を押しながら USB を接続して更新モードへ入る手順を案内します。この手順は資料にあるもので、未検証です

## 検証状況

観測結果と未検証の項目は [docs/analysis/findings.md](docs/analysis/findings.md) に記録しています。
