<!-- 言語切替（GitHubはJSタブ非対応のため、リンクを言語タブとして使います）。 -->
### 🌐 [English](README.en.md) · **日本語**

# StreamClock

OBSでの配信時オーバーレイ向け**デジタルクロック**
時計のサイズは自由に変更することが出来、背景を黒から透明まで設定できるため、
画面が狭くてもOBSなどにオーバーレイできるため邪魔にならない設計です。
さらに配信時に便利なストップウォッチ機能付き。
ライブでのプロンプターとして使用するなど長時間の連続稼働を想定した堅牢な Rust 製ネイティブアプリです。

外部時刻ソースにスレーブ可能です。（NTP / PTP / MTC / LTC）
LTC/MTCにロック可能なので、単純なタイムコードリーダーとしても使用できます。

**Windows / macOS** 両対応

MIT ライセンスのフリー／オープンソースです。

> **エディション** — どちらも**無料**:
> - **フル版**（Windows / macOS 直接配布・Homebrew）: System / NTP / PTP / MTC / LTC。
> - **ライト版**（macOS App Store・予定）: 時刻ソースを **System + NTP に限定**し、Sandbox に
>   適合させて審査を通すための版。App Store 版は署名済みのため、**Gatekeeper の警告が出ずに
>   インストール**できます（直接配布のフル版は未署名で、下記の回避が必要です）。
>
> 有料の iPad/iPhone 版は、別リポジトリ・別ライセンスの独立プロジェクトです。

---

## 表示

最大4段（上から）:

1. 日付 `YYYY/MM/DD`（小）— *表示ON/OFF可・既定ON*
2. 現在時刻 `HH:MM:SS`（大）
3. ストップウォッチ `HH:MM:SS`（大・状態で色が変化）
4. ステータス行 — 時刻ソースの同期状態（小）— *表示ON/OFF可・既定OFF*

文字はウィンドウサイズに連動して自動拡縮します（右下グリップでリサイズ）。
時刻は既定で **JST（UTC+9）**、PC のタイムゾーン設定に依存しません（ゾーンは設定で変更可）。

## 時刻ソース

**歯車アイコン**（ウィンドウにマウスを乗せると右上に表示）→ **Settings**、または右クリックメニューで切替。

| ソース | 説明 |
|--------|------|
| System | PC のシステム時計（UTC→選択ゾーン換算） |
| NTP | SNTP で定期同期（既定 `ntp.nict.jp`・64秒間隔）。オフセットを加算 |
| PTP | IEEE 1588 (PTPv2) UDP マルチキャスト受信（AES67 / SMPTE ST 2110）。受信専用 |
| MTC | MIDI タイムコード（クォーターフレーム＋フルフレーム SysEx・24/25/29.97/30 自動判別） |
| LTC | SMPTE LTC をオーディオ入力から復号（バイフェーズマーク・レベル/極性非依存） |

MTC/LTC は信号が2秒途切れるとフリーホイールを止めて最終値を保持し、赤の **NO SIGNAL** を表示します。
（PTP/MTC/LTC は**フル版のみ**）

## 操作

| 操作 | 動作 |
|------|------|
| ウィンドウにマウスを乗せる | 右上に**歯車アイコン**が出る → クリックで設定を開く |
| 文字エリア（ストップウォッチ以外）をドラッグ | ウィンドウ移動 |
| 右下グリップをドラッグ | サイズ変更（文字も連動拡大） |
| マウスホイール / ↑↓ | 背景（黒）の不透明度を調整 |
| **ストップウォッチをダブルクリック** | ストップ → リセット → スタート |
| 右クリック | メニュー（時刻ソース / 最前面 / 設定 / 終了） |
| Esc | 終了 |

背景の黒だけ透過し、文字は不透明のまま残ります。0% 時はマウスを乗せる/ドラッグすると枠線が出ます。

## 設定画面

- **時刻ソース**、NTP サーバー、PTP ドメイン
- **入力選択**: MTC 用 MIDI ポート / LTC 用オーディオ入力 / NTP 用 NIC / PTP 用 NIC
  （NIC 既定は Auto＝デフォルトルート）。OS が日本語などの非 ASCII でデバイス名を返しても正しく表示されます。
- **表示トグル**: 日付行の表示（既定ON）、フレーム表示（`…:FF`）を**時計用**と**ストップウォッチ用**で
  個別に（両方既定OFF）、4段目のステータス行の表示（既定OFF）。
- **タイムゾーン** — UTC オフセット（時間）、JST / UTC プリセット付き（既定JST）。
- ローカルフレームレート（タイムコードにスレーブしていないとき使用）。
- 文字色パレット（プリセット5色＋自由選択）、フォント: Modern / 7-Segment（DSEG7）。
- 背景不透明度、最前面表示。
- 「クローズ時に終了せずタスクバー/Dock へ最小化」（右クリック→Exit で完全終了）。

設定は自動保存され、次回起動時に復元されます（ウィンドウ位置・サイズ含む）。

## ビルド

[Rust](https://rustup.rs/) を導入のうえ:

```sh
cargo build --release
```

- **Windows**: `target\release\stream-clock.exe`（MSVC・リリースはコンソール非表示）
- **macOS**: `target/release/stream-clock`

### macOS ユニバーサル `.app`（両エディション）

`./deploy.sh` が**ユニバーサル (arm64 + x86_64)** の `.app` を両エディション分ビルドし、
フル版のインストールまで行います:

```sh
./deploy.sh build     # → dist/full/StreamClock.app ＋ dist/appstore/StreamClock.app
./deploy.sh install   # フル版を /Applications へ（または ./deploy.sh all）
```

`rustup`（`aarch64-apple-darwin` ＋ `x86_64-apple-darwin` ターゲット）と
`cargo install cargo-bundle` が必要です。フル版 `.app` の `Info.plist` には
`NSMicrophoneUsageDescription`（LTC 音声入力用）を付与しています。

直接配布の macOS フル版は**未署名**のため、初回は Gatekeeper にブロックされます。
右クリック→「開く」か、次を実行してください:

```sh
xattr -dr com.apple.quarantine /Applications/StreamClock.app
```

（App Store のライト版は署名済みのため、この警告は出ません。）

> Windows 11 の Smart App Control が有効だと、未署名バイナリ（cargo のビルドスクリプト含む）の
> 実行がブロックされます（os error 4551）。開発機ではオフにしてください。

## テスト

```sh
cargo test
```

LTC デコーダのラウンドトリップ（30fps/48kHz・25fps/44.1kHz・低レベル・極性反転・偽同期耐性）、
MTC クォーターフレーム組み立て、PTP パケット解析、NTP タイムスタンプ換算を検証しています。

## 構成

```
src/
  main.rs  — GUI (eframe/egui)・設定永続化・ストップウォッチ・NIC 列挙
  tc.rs    — SMPTE タイムコード型
  ntp.rs   — SNTP クライアント（自前実装）
  ptp.rs   — PTPv2 リスナー（自前実装）
  mtc.rs   — MTC 受信 (midir)        [full-sources フィーチャー]
  ltc.rs   — LTC 復号 (cpal＋バイフェーズマーク) [full-sources フィーチャー]
assets/fonts — DSEG7 Classic Bold（埋め込み）
legacy/    — 旧 PowerShell+WPF 版（v0.1・参照のみ）
deploy.sh  — macOS ユニバーサルビルド＋バンドル＋インストール
```

**ライト版 / App Store** ビルドは `cargo build --release --no-default-features` で生成し、
PTP/MTC/LTC とその依存（midir/cpal）をコンパイル時に除外して Sandbox 安全にします。

## ライセンス

本プロジェクトは **[MIT License](LICENSE)** です。

埋め込みフォント **DSEG7 Classic**（© 2017 keshikan）は **SIL Open Font License 1.1**
（`assets/fonts/DSEG-LICENSE.txt`）です。その他のサードパーティ構成要素は
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) に記載。別途の有料 iPad/iPhone 版は
独立したライセンスで、本ライセンスの対象外です。
