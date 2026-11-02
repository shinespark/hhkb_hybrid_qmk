# Spec Delta

## MODIFIED Requirements

### Requirement: 移行前に対象を確認する
CLI は、更新モードへの移行の前に、対象が USB の 1 台であること、公式ツールが実行中でないことを確認しなければならない（SHALL）。純正 FW の動作中は、応答の版が書き込む HFB の版と一致することも確認しなければならない（SHALL）。

#### Scenario: 版が一致しない
- **WHEN** 純正 FW の動作中に、実機の応答の版が HFB の版と異なる、または版を読み取れない
- **THEN** 更新モードへ移行せずに 0 以外で終了する

#### Scenario: 対象が 1 台ではない
- **WHEN** USB の対象が 0 台または 2 台以上である
- **THEN** レポートを送信せずに 0 以外で終了する

#### Scenario: QMK の動作中
- **WHEN** QMK の動作中に更新モードへの移行を実行する
- **THEN** 版を確認するレポートを送らずに、移行の手順へ進む

## ADDED Requirements

### Requirement: QMK の動作中は QMK のインターフェースで移行する
CLI は、QMK の動作中の移行で、Usage Page `0xFF60` のインターフェースだけに、32 バイトのレポートを送らなければならない（SHALL）。

#### Scenario: QMK の動作中の移行
- **WHEN** QMK の動作中に更新モードへの移行を実行する
- **THEN** `0xFF60` のインターフェースだけを開き、送信するレポートは 32 バイトである

### Requirement: HFB の種類で移行のコマンドを選ぶ
CLI は、QMK の動作中に QMK の HFB を書き込むときは `e0` を、純正の HFB を書き込むときは `f0` を送らなければならない（SHALL）。純正 FW の動作中は `e0` を送らなければならない（SHALL）。

#### Scenario: QMK から QMK へ
- **WHEN** QMK の動作中に QMK の HFB で移行する
- **THEN** `e0` を送り、`f0` を送らない

#### Scenario: QMK から純正へ
- **WHEN** QMK の動作中に純正の HFB で移行する
- **THEN** 最初に `f0` を送る

### Requirement: 純正へ戻すときに VIA の設定を残さない
CLI は、`f0` が拒否されたとき、VIA の問い合わせに応答がない場合だけ `e0` へ進まなければならない（SHALL）。VIA が応答する場合は、更新モードへ移行してはならない（MUST NOT）。

#### Scenario: VIA に対応しない QMK
- **WHEN** `f0` が拒否され、VIA の問い合わせに応答がない
- **THEN** `e0` を送って移行する

#### Scenario: `f0` を持たない VIA 対応の QMK
- **WHEN** `f0` が拒否され、VIA の問い合わせに応答がある
- **THEN** 移行せずに理由と次の手順を表示して、0 以外で終了する

### Requirement: QMK の動作中は本体を書き込まない
CLI は、QMK の動作中に見えるとき、本体の書き込みを始めてはならない（MUST NOT）。

#### Scenario: QMK の動作中に書き込みを実行する
- **WHEN** USB の製品名やインターフェースが QMK の動作中を示す状態で本体の書き込みを実行する
- **THEN** レポートを送信せず、先に移行するよう案内して 0 以外で終了する

### Requirement: 書き込み先の種類を実行前に示す
CLI は、計画と確認文の表示で、HFB の種類（純正、QMK の `default`、QMK の `via`）を示さなければならない（SHALL）。

#### Scenario: 計画の表示
- **WHEN** 許可された HFB で計画を実行する
- **THEN** HFB の種類が表示される

### Requirement: QMK が応答しないときの案内
CLI は、QMK の動作中の移行で中止したとき、Esc を押しながら USB を接続して更新モードへ入る手順を、未検証であることとあわせて表示しなければならない（SHALL）。

#### Scenario: QMK の移行の応答がない
- **WHEN** QMK の動作中に移行のレポートの応答がない
- **THEN** 実機の状態を断定せず、手順と、未検証であることを表示する
