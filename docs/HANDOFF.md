# StreamClock 引き継ぎ

## 最新の開発再開情報（2026-11-27）

[詳細handoff](HANDOFF-2026-11-27.md)を最初に読むこと。Windows で背景が透明にならない問題の原因、修正、ピクセル実測の結果、インストール状態を収録。

- 主因は描画設定。wgpu の既定（Vulkan、または DX12 の HWND スワップチェーン）は不透明でしか合成されない。Windows では DX12 + DirectComposition（`DxgiFromVisual`）に固定して解決した（`src/main.rs` の `windows_transparent_wgpu()`）。
- ホイールは 1 ノッチ = 5% に修正した（`wheel_notches()`）。以前は 1 ノッチで 50〜65% 動いていた。
- ユーザーが実機で確認済み。修正は作業ブランチ `claude/scroll-transparency-bug-d94f2a` に push 済みで、**main 未マージ**。

**未完了:** main へのマージ、OBS のウィンドウキャプチャでの見え方の確認、App Store 構成と macOS での回帰確認。

## 2026-10-17: アイコンとショートカットの復旧

[詳細handoff](HANDOFF-2026-10-17.md)。アイコン採用の経緯、コード変更、ビルド・配備・診断の証拠、実際のデスクトップへのショートカット作成、Git同期方針を収録。

**未完了:** タスクバーのピン留めと表示確認。デスクトップのショートカットは作成済み。アプリ本体は残っており、インストールEXEとビルド成果物のSHA256一致を2026-10-17に再確認した。

## 2026-09-15: アイコン刷新

- GitHub の `a919a3a`（ソース v1.0.2）を基点に作業。履歴書き換え前のローカル main は内容一致を確認し、`codex/backup-before-sync-20260915` にローカル退避した。
- ユーザーが5案からオレンジ色のストップウォッチを採用。原稿は `design/icon-concepts/2026-09-15/04-stopwatch.png`。他案と生成プロンプトも同ディレクトリに保存。
- サブエージェントは使用していない。
- `assets/icon/icon_1024.png`、`app.ico`、`AppIcon.icns` を更新。PNGは1024px、ICOは16/24/32/48/64/128/256px、ICNSは標準・Retinaサイズを収録。
- `src/main.rs` の ViewportBuilder にアイコンを設定し、起動中も採用アイコンを使う。
- `build.rs` に ICO の変更監視を追加。Windows実行ファイルのアイコンリソースを再ビルドする。
- 再書き出し: `./scripts/export-icon.ps1 -SourcePath ./design/icon-concepts/2026-09-15/04-stopwatch.png`（Windows PowerShell / System.Drawing）。デザインの再生成は不要。

## 検証・配布

- PNG、ICO、ICNSを読み込み、ICO/ICNSの全収録サイズをデコードして確認済み。
- Windows: `cargo build --release` 成功。既存コードに由来する警告7件あり。
- このマシンの `%LOCALAPPDATA%/Programs/StreamClock/StreamClock.exe` と隣の `app.ico` を更新済み。インストール後のSHA256がビルド成果物・ICO原本と一致することを確認。
- 旧実行ファイルとICOは `%LOCALAPPDATA%/StreamClock/backups/20260915-092903/` に退避。
- インストールした実行ファイルを起動し、プロセスの稼働・応答とウィンドウ生成を確認。
- タスクバーが白いとの報告後、実行ファイルから抽出した32pxアイコンと実行中ウィンドウの20/40pxアイコンが正常なオレンジ色であることを確認。インストール済みEXEもビルド成果物と一致。
- ローカルのスタートメニューショートカットは、EXE内リソース参照から `%LOCALAPPDATA%/Programs/StreamClock/streamclock-orange-3bb557c.ico,0` へ変更。元のICOを別名でコピーし、Shell変更通知と `ie4uinit.exe -show` でアイコン再読み込みを要求した。タスクバーの最終表示はユーザー側での確認待ち。
- macOSのバンドルは Cargo.toml が更新済み `AppIcon.icns` を参照する。Windows環境ではmacOSビルド・Dock表示・App Store検証は未実施。Mac側で `./deploy.sh build` と確認が必要。
- バージョンは1.0.2のまま。今回のGit pushはソース更新であり、GitHub ReleaseやApp Storeへの公開は含まない。
