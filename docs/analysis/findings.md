# 解析記録

この記録は非公式で無保証です。法的な助言ではなく、記載の行為が適法であることや使用許諾に違反しないことを保証しません。詳細は [README の免責事項](../../README.md#免責事項) を参照してください。

各項目の状態は次のいずれかです。

- **観測済み**: 手元の実機で確認し、再現手順がある
- **仮説**: 外部の資料にしか根拠がない
- **不一致**: 観測と資料が食い違う

## 接続情報

再現コマンド（どちらもキーボードにレポートを送信しません）:

```sh
cargo run -p hhkb -- list
hidutil list --matching '{"VendorID":0x4fe}'
```

| 項目 | 内容 | 状態 | 根拠 |
|---|---|---|---|
| USB ID | `04fe:0021` | 観測済み | `hhkb list`、`hidutil list` |
| USB の製品名 | `HHKB-Hybrid` | 観測済み | `hhkb list`、`hidutil list` |
| USB のバージョン | `0x0001` | 観測済み | `hhkb list` |
| キーボード用インターフェース | Usage Page `0x0001` / Usage `0x0006` | 観測済み | `hhkb list`、`hidutil list` |
| 複合インターフェース | Usage `0x000c/0x0001` と `0x0001/0x0006` を持つ | 観測済み | `hhkb list` |
| ベンダー定義インターフェース | Usage Page `0xff00` / Usage `0x0001` | 観測済み | `hhkb list`、`hidutil list` |
| Bluetooth 側の表示 | 同じ ID で製品名 `HHKB-Hybrid_1` | 観測済み | `hidutil list` |
| Bluetooth 側の除外 | `hhkb list` は USB のインターフェースだけを表示する | 観測済み | Bluetooth 接続中の `hhkb list` |
| 更新用インターフェース | `0xff00` を更新に使う | 仮説 | [l3tnun/hhkb_hybrid の device-state.md](https://github.com/l3tnun/hhkb_hybrid/blob/main/docs/device-state.md) |
| 純正 FW と更新モードの区別 | USB の表示では区別できず、キー入力の可否で見分ける | 仮説 | [l3tnun/hhkb_hybrid の device-state.md](https://github.com/l3tnun/hhkb_hybrid/blob/main/docs/device-state.md) |

macOS の `hidapi` が返すパス `DevSrvsID:<値>` の値は、`hidutil list` の RegistryID を 10 進数にしたものと一致します。この値は再接続で変わる可能性があるので、機器の識別には使いません。

## ユーザーの承認

| 対象 | 結果 |
|---|---|
| 公式ツールへの dyld 差し替え | 承認済み。実機に触れる記録は段階ごとに別途承認を得る |
| 通信と HFB の解析 | 了承済み。自分のキーボードの通信と公式更新ファイルの構造に限り、逆アセンブルは行わない |
| 解析結果の公開範囲 | Public。純正 HFB、生ログ、`.hks`、個体情報は公開しない |

## ロガーの動作確認

実機と公式ツールに触れず、NULL デバイスだけを扱う自作の実行ファイルで確認しました。

再現コマンド:

```sh
cargo build -p hid-logger --examples
cargo build -p hid-logger
DYLD_PRINT_INTERPOSING=1 HHKB_HID_LOGGER_PATH=local/probe.log HHKB_HID_LOGGER_MODE=probe DYLD_INSERT_LIBRARIES=$PWD/target/debug/libhid_logger.dylib target/debug/examples/interpose_probe
```

| 項目 | 結果 | 状態 |
|---|---|---|
| `IOHIDDeviceSetReport` の差し替え | dyld の出力で確認 | 観測済み |
| `probe` での送信の停止 | 送信前に記録して中止（終了コード 134） | 観測済み |
| 保存先の未指定での送信の停止 | `firmware-update` を指定しても中止 | 観測済み |
| ログの権限 | `0600` | 観測済み |
| 公式ツールへの読み込み | `scripts/run-official-tool.sh --expect-no-device probe` で読み込みと記録の開始を確認（キーボードなし） | 観測済み |
| 公式ツールの署名 | 記録の前後で `codesign --verify --deep --strict` に成功 | 観測済み |
| 公式ツールでの差し替え | 実機との送受信を記録できた | 観測済み |
| 実物の IOKit への転送、入力コールバック | 送信の戻り値はすべて `0` で、応答も公式ツールに届いた | 観測済み |
| 安全弁による中止 | `probe` で最初のレポートを送らずに中止した。その後もキー入力できた | 観測済み |

## 記録開始前の操作

通信の記録を始める前に、ユーザーが公式ツールでキーマップの書き込みを 3 回行いました。レポートの中身は記録していません。

## 公式ツールのログから分かった順序

公式ツールのログ（`~/Library/Application Support/hhkb-keymap-tool/`）で確認した順序です。ログには番号だけがあり、レポート内の位置と内容は未確認です。ログにはシリアル番号が含まれるので、内容は転記しません。

| 操作 | コマンドの順序 | 状態 |
|---|---|---|
| 起動時（操作なし） | `01 → 02 → 05 → 87×4 → 06 → 09 → 01` | 観測済み |
| キーマップの書き込み | `01 → 02 → 05 → 06 → 86×4 → 08 → 04 → 07 → 01` | 観測済み |

## レポートの形式

`probe` モードで、最初のレポートを実機に送らずに記録しました。

```sh
./scripts/run-official-tool.sh probe
```

| 項目 | 内容 | 状態 |
|---|---|---|
| 送信先 | Usage Page `0xff00`、Output レポート、レポート ID `0` | 観測済み |
| 長さ | 64 バイト | 観測済み |
| 最初のレポート | `aa aa 01 00 01` に続けてすべて `00` | 観測済み |
| 送信の先頭 2 バイト | すべて `aa aa` | 観測済み |
| コマンドの位置 | 3 バイト目（オフセット 2） | 観測済み |

### 読み出しの記録

`read` モードで、起動から正常終了までを記録しました。公式ツールの署名は前後とも検証に成功し、キーボードのキー入力とは別に、`0xff00` の通信だけを全バイト記録しています。

```sh
./scripts/run-official-tool.sh read 2
```

送信（64 バイト、残りは `00`）:

| コマンド | 公式ツールのログの関数名 | 送信の先頭 | 応答数 | 応答の長さ |
|---|---|---|---|---|
| `01` | `NotifyApplicationState` | `aa aa 01 00 01 00`（開始）、`aa aa 01 00 01 01`（終了） | 1 | 0 |
| `02` | `GetKeyboardInformation` | `aa aa 02` | 1 | 57 |
| `05` | `GetDipswInformation` | `aa aa 05` | 1 | 12 |
| `87` | `GetKeymap(mode:fnKey:isShortcut:)` | `aa aa 87 00 02 <mode> <fn>` | 3 | 58、58、12 |
| `06` | `GetKeyboardMode` | `aa aa 06` | 1 | 1 |
| `09` | `GetSleepSetting` | `aa aa 09` | 1 | 1 |

応答（64 バイト）の形式:

| 位置 | 内容 | 状態 |
|---|---|---|
| 0〜1 | `55 55` | 観測済み |
| 2 | 送信と同じコマンド | 観測済み |
| 3 | 今回はすべて `00`。結果コードと推定 | 仮説 |
| 4 | 分割なしは `00`、3 分割は `41`、`82`、`c3`。分割の番号と推定 | 仮説 |
| 5 | データ長 | 観測済み（残りがすべて `00` であることと、合計 128 バイトで確認） |
| 6〜 | データ | 観測済み |

`.hks` との照合（値は記録しません）:

| 項目 | 結果 | 状態 |
|---|---|---|
| `87` の `mode=01 fn=1` | `WithFn` と 128 バイトすべて一致 | 観測済み |
| `87` の `mode=01 fn=0` | `Normal` と 1 バイト（位置 50）だけ異なる。原因は未確認 | 不一致 |
| `87` の `mode=11` | `fn` の値によらず同じデータで、`.hks` とは異なる。意味は未確認 | 観測済み |
| `05` | 先頭 6 バイトが `DipSwitch` と一致 | 観測済み |
| `09` | データ長は 1 で、`SleepSetting` の値と直接は一致しない。符号化は未確認 | 不一致 |

## 実機の機種と版

公式ツールのログで確認しました。後続の判断の根拠になるので記録します。

| 項目 | 内容 | 状態 |
|---|---|---|
| 型番 | `PD-KB800BNS` | 観測済み |
| アプリ領域の版 | `A0.48` | 観測済み |
| ブート領域の版 | `B0.26` | 観測済み |
| `RunningFirmwareState` | 通常動作中の値は `0` | 観測済み |

## 次の段階へ進む条件

### この change で確定するもの

| 条件 | 状況 |
|---|---|
| 読み取りコマンドの形式を確認した | 充足（`mode=11`、`09` の意味、`Normal` の 1 バイト差は未確認） |
| HFB の構造を確認した | 未充足 |
| 公式ツールでの純正の再書き込みを記録した | 未充足 |

### 次の検証で確定するもの

| 条件 | 確定するタイミング | 失敗時の対処 |
|---|---|---|
| 自作ツールから macOS で更新できる | 書き込みの change | 公式ツールで純正に戻す |
| QMK の HFB が受理され、起動する | 書き込みの change | 更新モードから純正に戻す |
| macOS だけで純正に戻せる | 書き込みの change | Linux 環境を用意する。最終手段は分解 |
