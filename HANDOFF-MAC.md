# HANDOFF-MAC — macOSビルド担当LLMへの伝達事項

最終更新: 2026-06-13（Windows側 Claude より）
まず [HANDOFF.md](HANDOFF.md)（全体アーキテクチャ・設計判断）を読んでから本書に進んでください。

## 結論サマリ

コードはクロスプラットフォームのクレートのみで書かれており、**macOS固有の修正なしでビルドが通る想定**です。
`cargo build --release` と `cargo test`（27件）がそのまま動くはずです。
プラットフォーム依存はクレート側が吸収します: GUI=eframe(AppKit/Metal) / MIDI=midir(CoreMIDI) / 音声=cpal(CoreAudio) / NIC列挙=if-addrs。

## セットアップ手順

```sh
# 1. Xcode Command Line Tools（リンカとシステムフレームワーク）
xcode-select --install

# 2. Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 3. クローン（privateリポジトリなので gh 認証が必要）
gh auth login --web
gh repo clone masatomoota/win-stream-clock
cd win-stream-clock

# 4. ビルド・テスト
cargo test            # 27件全パスを確認してから
cargo build --release # → target/release/stream-clock
```

Apple Silicon なら `aarch64-apple-darwin`（rustupが自動選択）。Intel Mac 配布も必要なら
`rustup target add x86_64-apple-darwin` → 両方ビルド → `lipo -create` でユニバーサル化。

## macOS固有の注意点（重要度順）

1. **マイク権限（LTC受信に必須）**: cpalの音声入力はTCC（プライバシー）の対象。
   ターミナルから起動した場合は初回にターミナル.app（またはiTerm等）へのマイク許可ダイアログが出る。
   拒否されるとLTCデバイスのストリーム開始がエラーになる。`システム設定 > プライバシーとセキュリティ > マイク` で確認。
   **.appバンドル化する場合は Info.plist に `NSMicrophoneUsageDescription` が必須**（ないと即クラッシュ）。

2. **PTPの低番ポート (319/320)**: macOS 10.14 Mojave 以降は非rootで1024未満のポートをbind可能なので
   通常は問題ないはず。もし `Permission denied` が出たら sudo で挙動確認のうえ報告してほしい。
   他のPTPデーモン（例: AVB/Thunderboltブリッジの `ptpd`）が既にポートを掴んでいる可能性もある
   （`sudo lsof -i :319` で確認。コードは SO_REUSEADDR を設定済みだが、排他になるケースあり）。

3. **ファイアウォール**: NTP/PTPのUDP受信をmacOSのアプリケーションファイアウォールが
   ブロックすることがある。初回起動時の「着信接続を許可」ダイアログで許可する。

4. **ウィンドウ透過・最前面・ドラッグ**: eframe/winit は macOS の透過NSWindow・
   always-on-top・undecoratedドラッグをサポート済み。動くはずだが、
   **背景不透明度0%時の枠線表示（マウスオーバー/ドラッグ時）と右下グリップでのリサイズ**は実機確認してほしい。

5. **「タスクバーに残る」オプション**: Windows用語なのでmacでは「Dockに残る」挙動になる
   （`ViewportCommand::Minimized(true)` = Dockへ最小化）。動作自体はそのままで問題ない見込み。

6. **設定の保存先**: eframe persistence → `~/Library/Application Support/stream-clock/`。
   Windowsとは別管理なので初回はデフォルト設定で起動する。

7. **NIC選択のAuto判定**: `default_route_ip()` は UDP connect トリック
   （8.8.8.8:80へconnect→local_addr、送信なし）。macでは en0 等が返るはず。
   設定画面の「Auto (default route: x.x.x.x)」表示が実際のプライマリNICと一致するか確認。

## 配布用 .app バンドル化（任意・動作確認後でよい）

素のバイナリはFinderダブルクリックで起動できない（ターミナルからは可）。配布するなら:
- `cargo install cargo-bundle` → `Cargo.toml` に `[package.metadata.bundle]` を追記して `cargo bundle --release`
- または手動で `StreamClock.app/Contents/{MacOS,Info.plist}` を構成
- **Info.plist に必ず `NSMicrophoneUsageDescription` を入れる**（LTC用）
- 他マシンへ配るならGatekeeper対策（`xattr -dr com.apple.quarantine` 案内 or codesign/notarize）

## 検証チェックリスト（実機で順に）

- [ ] `cargo test` 27件全パス
- [ ] 起動・4段表示・ウィンドウリサイズで文字が連動拡大
- [ ] 背景不透明度の調整（ホイール/↑↓）、0%時のマウスオーバーで枠線
- [ ] ドラッグ移動、右下グリップでリサイズ
- [ ] ストップウォッチのダブルクリック（開始→停止→リセット）と、秒の繰り上がりが時刻と同期していること
- [ ] 設定パネルが黒背景+白文字で読めること（macのライト/ダーク外観の両方で）
- [ ] NTP同期（ステータス行に offset/delay 表示）
- [ ] MIDIポート列挙（IACドライバを有効にすると無デバイスでもポートが作れる）
- [ ] オーディオ入力列挙とマイク権限ダイアログ
- [ ] NIC列挙とAuto表示
- [ ] フォント切替（DSEG7は assets/ に同梱・埋め込み済み、フォントインストール不要）
- [ ] 「Minimize on close」ONでEsc→Dockに最小化、右クリックExitで完全終了

## 開発規約（Windows側と共通）

- 新しい Settings フィールドには必ず `#[serde(default)]`（旧設定との互換維持）
- テストを壊さない・減らさない。挙動変更にはテストを足す
- コミット: サブジェクト英語+本文日本語、`Co-Authored-By: Claude <model> <noreply@anthropic.com>`
- mac固有の修正が必要になった場合は `#[cfg(target_os = "macos")]` で分岐し、Windows側の挙動を変えないこと。
  修正内容はこのファイルの末尾に追記して同じブランチ(main)にpushしてください
- 実装はSonnet等の安価なモデルのサブエージェントに委任し、上位モデルは管理・レビューに徹する方針（ユーザー指示）

## macOS側での変更履歴（mac LLMが追記すること）

（まだなし）
