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

## 配布方法の選択肢（検討済み）

### 1. Homebrew formula（無料・フル機能版推奨）

署名・公証不要。技術者・プロ向けに最適。`masatomoota/homebrew-stream-clock` tap を作成:

```ruby
class StreamClock < Formula
  url "https://github.com/masatomoota/win-stream-clock/archive/refs/tags/v0.2.0.tar.gz"
  depends_on "rust" => :build
  def install
    system "cargo", "install", "--root", prefix, "--path", "."
  end
end
```

ユーザー: `brew tap masatomoota/stream-clock && brew install stream-clock`

### 2. DMG直配布（署名なし）

`.app` バンドルを作成して DMG に詰める場合:

```sh
cargo install cargo-bundle
# Cargo.toml に [package.metadata.bundle] を追記後:
cargo bundle --release
```

Info.plist に **`NSMicrophoneUsageDescription`** が必須（LTC用オーディオ入力）。  
署名・公証なしでも動くが Gatekeeper がブロックするため README に案内:

```sh
xattr -dr com.apple.quarantine /Applications/StreamClock.app
# または Finder で右クリック→「開く」
```

### 3. Mac App Store版（一般配信者向け・検討中）

- 費用: $99/年 + 売上の15〜30%（Apple Developer Program）
- **Sandbox制約**: MTC/LTC/PTP は対応困難。**NTP + System 限定**なら Sandbox を通過可能
- `com.apple.security.network.client` entitlement で outbound UDP（port 123）は許可される
- 一般配信者向けシンプル版として有料（¥250〜¥600程度）で出す戦略を検討中
- フル機能版（brew）と2本立てで市場を分ける想定

### 4. iOS/iPad版（将来検討）

- eframe 0.34 は **iOS 未対応**。GUI 層を SwiftUI で書き直す必要あり
- Rust の NTP ロジックは C FFI 経由で再利用可能（`cbindgen` 等）
- **iPadOS 16以降**をターゲット → 2019年以降の iPad（mini 5+, Air 3+, Pro）をカバー
- Mac App Store 版が軌道に乗ってから検討する想定

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
