# hfb-generation Specification

## Purpose

許可した公式 HFB をテンプレートに、許可した QMK の `.bin` から、構造と整合性の検査に通る HFB を生成する。生成物は手元での利用に限る。

## Requirements

### Requirement: 許可した入力だけから生成する
CLI は、SHA-256 が許可リストと一致し、整合性の検査に通った公式 HFB と、SHA-256 が許可リストと一致する `.bin` だけから HFB を生成しなければならない（SHALL）。検査を省略する手段を持ってはならない（MUST NOT）。

#### Scenario: 許可リストにないテンプレート
- **WHEN** SHA-256 が許可リストと異なる HFB をテンプレートに指定する
- **THEN** 理由を表示し、ファイルを作らずに 0 以外で終了する

#### Scenario: 許可リストにない `.bin`
- **WHEN** SHA-256 が許可リストと異なる `.bin` を指定する
- **THEN** 理由を表示し、ファイルを作らずに 0 以外で終了する

### Requirement: アプリ領域だけを置き換える
CLI は、テンプレートの領域 A を `.bin` で置き換え、余りを `0xFF` で埋めなければならない（SHALL）。2 つの CRC を計算し直す以外に、領域 A より後の内容を変えてはならない（MUST NOT）。

#### Scenario: 領域の比較
- **WHEN** HFB を生成する
- **THEN** 領域 A の先頭は `.bin` と一致し、余りはすべて `0xFF` で、オフセット `0x10004` 以降はテンプレートと全バイト一致する

#### Scenario: `.bin` が領域 A より大きい
- **WHEN** `.bin` が `0x10000` バイトより大きい
- **THEN** 理由を表示し、ファイルを作らずに 0 以外で終了する

### Requirement: 生成物を検査する
CLI は、生成した内容が構造と整合性の検査に通ることを確かめてから、ファイルに書き出さなければならない（SHALL）。

#### Scenario: 生成物の検査
- **WHEN** HFB を生成する
- **THEN** `hhkb inspect` の検査で全体 CRC、領域 A CRC、領域 B MD5 がすべて一致する

### Requirement: 生成物を保護する
CLI は、生成した HFB を所有者だけが読み書きできる権限で `local/` に作らなければならない（SHALL）。既存のファイルを上書きしてはならない（MUST NOT）。

#### Scenario: 権限の確認
- **WHEN** HFB を生成する
- **THEN** ファイルの権限は 0600 で、`git status` に現れない

#### Scenario: 出力先にファイルがある
- **WHEN** 出力先に同じ名前のファイルがある
- **THEN** 理由を表示し、既存のファイルを変更せずに 0 以外で終了する

### Requirement: 生成では実機に触れない
HFB を生成する CLI は、HID デバイスを開いてはならない（MUST NOT）。

#### Scenario: 実機を接続したまま生成する
- **WHEN** 実機を接続した状態で HFB を生成する
- **THEN** 実機にレポートは送信されない
