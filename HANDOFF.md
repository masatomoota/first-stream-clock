# HANDOFF — StreamClock 開発引き継ぎ

最終更新: 2026-06-13

## プロジェクト概要

配信オーバーレイ向けの JST デジタルクロック。Rust + eframe/egui 0.34。
長時間連続稼働を前提とし、外部時刻ソース (NTP / PTP / MTC / LTC) にスレーブできる。
Windows / macOS クロスプラットフォーム（プラットフォーム固有コードなし）。

- リポジトリ: https://github.com/masatomoota/win-stream-clock (private)
- ビルド: `cargo build --release` → `target/release/stream-clock.exe`（単体配布可、コンソール非表示）
- テスト: `cargo test`（現在 27 件、全パス必須）

## アーキテクチャ

```
src/main.rs  GUI・設定永続化(eframe storage/serde)・ストップウォッチ・NIC列挙
src/tc.rs    Timecode型 {h,m,s,f} + advanced_by(フリーホイール用) + nominal_fps
src/ntp.rs   SNTPクライアント(自前実装)。専用スレッド+mpsc(NtpCmd)+Arc<Mutex>status
src/ptp.rs   PTPv2リスナー(自前実装)。Sync/Follow_Up/Announce解析、TAI→UTC換算
src/mtc.rs   MTC受信(midir)。クォーターフレーム組み立て+フルフレームSysEx
src/ltc.rs   LTC復号(cpal)。バイフェーズマーク自前デコーダ(テスト内エンコーダで検証)
assets/fonts DSEG7 Classic Bold (SIL OFL) — include_bytes!でバイナリ埋め込み
legacy/      旧PowerShell+WPF版(v0.1) — 参照のみ、メンテ対象外
```

### スレッドモデル
- UI: メインスレッド、20fps repaint (`request_repaint_after(50ms)`)
- NTP: 専用スレッド、64秒間隔同期。`NtpCmd::{SetServer, SyncNow, SetBindIp}`
- PTP: event(319)/general(320) 2スレッド。domain/bind_ip は Atomic 経由で動的変更→ソケット再生成
- MTC: midir コールバック → Arc<Mutex> 共有
- LTC: cpal 入力コールバック → デコーダ → Arc<Mutex> 共有

### 表示仕様の要点
- 4段: 日付(小) / 現在時刻(大) / ストップウォッチ(大) / ステータス行(小)
- 全段 Viewbox 的スケーリング: `scale = min(w/480, h/300)`。フレーム表示時は時刻行×0.72、7セグ時は全行×0.80
- **ストップウォッチの秒繰り上がりは時刻行の秒境界に位相同期**（`aligned_display_secs`、停止時は表示値でacc固定→クリック瞬間の飛びなし）。内部は`Instant`単調時計
- MTC/LTC: 受信断2秒でフリーホイール停止、NO SIGNAL赤表示・最終値保持。フレームレートは受信から自動判別(24/25/29.97/30)
- System/NTP/PTP のフレーム番号はサブ秒×`local_fps`設定から算出
- 文字色は基準色(text_color)から bright/dim(×0.55)/dark(×0.35) を導出。橙(停止)・赤(NO SIGNAL)は固定の状態色

### 設定 (Settings struct, serde永続化)
新フィールドは必ず `#[serde(default)]` を付ける（旧設定との後方互換のため）。
source / ntp_server / ptp_domain / mtc_port / ltc_device / bg_alpha / topmost /
show_frames / local_fps / text_color / font_style / minimize_on_close / ntp_nic / ptp_nic

### NIC選択 (NTP/PTP)
- `None` = Auto = デフォルトルートNIC（最上位メトリック）。実装は UDP connect トリック
  (`connect 8.8.8.8:80 → local_addr()`、パケット送信なし)
- 明示選択時は NTP=bind、PTP=`join_multicast_v4(maddr, iface_ip)`
- NIC列挙は `if-addrs` クレート(IPv4のみ、ループバック除外)

## 重要な設計判断・注意点

1. **UTF-8 BOM不要(Rust)**。旧PS版はBOM必須だった(5.1のSJIS解釈)
2. **Windows 11 Smart App Control** が有効だと cargo のビルドスクリプト実行がブロックされる
   (os error 4551)。開発機では SAC をオフにする必要あり（再有効化はWindows再インストールのみ）
3. egui はOSテーマ追従するため `set_visuals(Visuals::dark())` を強制（ライトテーマ機で設定パネルが白背景になる事故防止）。設定Windowは fill(18,18,18) も明示
4. eframe 0.34 API: `update()`ではなく`logic()`+`ui()`、`smooth_scroll_delta`、
   `close_requested()`+`CancelClose`で閉じる動作を横取り(最小化オプション)
5. PTPはリスナー専用(Delay_Req送信なし)。オフセットに片道遅延を含む(LANではサブms)
6. LTCデコーダはゼロクロス間隔の適応閾値方式。極性・振幅・DCオフセット非依存。
   テストは encode→decode ラウンドトリップ(30fps/48k, 25fps/44.1k, 低振幅, 極性反転)
7. MTCクォーターフレームは8ピース完成時に+2フレーム補正(規格上の遅延)
8. 7セグフォント(DSEG7)にはスラッシュのグリフがない → 7セグ時は日付をダッシュ区切りに

## 開発体制メモ

- ユーザー方針: **実装はSonnetサブエージェントに委任し、Opusは管理役**(API費用のため)
- コミットは日本語本文+英語サブジェクト、`Co-Authored-By: Claude <model> <noreply@anthropic.com>`
- push先: origin/main (https://github.com/masatomoota/win-stream-clock.git, gh CLI認証済み)

## 未検証・既知の課題

- [ ] MTC/LTC/PTP の実信号での動作検証（手元に発生源がなく未実施）
- [ ] LTC: ドロップフレーム表記(";"区切り)は未対応(内部でDFビットは解析済み)
- [ ] PTP: ハードウェアタイムスタンプなし(ユーザー空間受信時刻)
- [ ] macOS 実機でのビルド・動作確認は未実施
- [ ] GitHub Releases へのバイナリ配布は未設定（希望があれば v0.2.x タグ + Release作成）
