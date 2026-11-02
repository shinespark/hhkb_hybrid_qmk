# Spec Delta

## MODIFIED Requirements

### Requirement: 対象が 1 台のときだけ通信する
CLI は、Usage Page `0xFF00` または `0xFF60` のインターフェースの数を台数とし、ちょうど 1 台でない場合は何も送信してはならない（MUST NOT）。

#### Scenario: 対象が見つからない
- **WHEN** USB で接続された対象がない
- **THEN** 接続を確認するよう表示し、終了コードは 0 以外になる

#### Scenario: 複数台の接続
- **WHEN** USB で接続された対象が 2 台以上ある
- **THEN** 何も送信せず、1 台にするよう表示し、終了コードは 0 以外になる

#### Scenario: 1 台が複数のインターフェースを持つ
- **WHEN** 1 台のキーボードが 3 つのインターフェースとして見えている
- **THEN** 1 台として扱う

#### Scenario: QMK の動作中
- **WHEN** 1 台のキーボードが `0xFF60` のインターフェースを持つ
- **THEN** 1 台として扱う

## ADDED Requirements

### Requirement: QMK の動作中を区別して表示する
CLI は、USB の製品名に `QMK` を含み、`0xFF60` のインターフェースを持つときは、QMK の動作中の可能性が高いと表示しなければならない（SHALL）。

#### Scenario: QMK の表示
- **WHEN** QMK の動作中に一覧表示を実行する
- **THEN** QMK の動作中の可能性が高いことが表示され、レポートは送信されない

#### Scenario: 判断材料が一致しない
- **WHEN** 製品名とインターフェースのどちらか一方だけが QMK を示す
- **THEN** 状態を確定できないと表示し、書き込みの段階に進まない

### Requirement: QMK には固定レポートを送らない
状態確認で、CLI は、QMK の動作中に見えるときはレポートを送信してはならない（MUST NOT）。

#### Scenario: QMK の動作中の状態確認
- **WHEN** QMK の動作中に状態確認を実行する
- **THEN** 一覧の情報だけを表示し、デバイスを開かない
