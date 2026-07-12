# winit windowing バックエンド スパイク(leg 1)

調査日: 2026-07-12(worker セッションによるスパイク)。目的: `docs/roadmap.md` の
「winit windowing backend (spike)」の leg 1 ── gpui を、gpui_linux 自身の
wayland/x11 実装ではなく winit が作った `ActiveEventLoop`/`Window` の上で走らせられるか、
`Application::with_platform` 経由(fork なし)で実測する。狙いは Wayland/GNOME での
sctk-adwaita クライアントサイド装飾(CSD)── これが本スパイクの戦略上の核心。

**プロトタイプ**: `spikes/gpui-winit/`(root workspace 非メンバー、`spikes/gpui-terminal/` と
同じ構え)。`gpui`/`gpui_wgpu` は Horizon のピンと同じ rev
`5f8a7413a31769e0882357f90dc424b3962ac72d` に明示固定(`spikes/gpui-terminal/` と違い
`rev =` を付けている ── root の未固定 git 依存とは独立に本スパイクの結論が後で
ずれないようにするため)。winit は `"0.30"`(解決先 `0.30.13`)。

## 0. 到達レベル

タスク指示の a〜d を**すべて満たした**。壁にはぶつからず、3 build-debug サイクル
の stop-loss には到達していない(実際にビルドが壊れた区間は §5 の型不一致数件のみで、
いずれも1サイクルで解消)。

| 基準 | 結果 | 証拠 |
|---|---|---|
| a. Wayland/GNOME で装飾が見える | **満たす** | §2 |
| b. `Application::run` がパニックせず走る | **満たす** | §3 |
| c. gpui view が gpui_wgpu 経由で描画される | **満たす** | §4 |
| d. キー入力が gpui に届く | **満たす** | §6 |

## 1. 実行環境

- Wayland セッション(`XDG_SESSION_TYPE=wayland`, GNOME/Mutter, `WAYLAND_DISPLAY=wayland-0`)。
- GPU: AMD Radeon 890M(RADV/Vulkan バックエンドを wgpu が自動選択)。
- 実行はすべてオーナーの実デスクトップ上(Xvfb ではない)。共有デスクトップでの
  スクリーンショット事故については §7 に記録。

## 2. 基準 a ── Wayland/GNOME での装飾

**結論: sctk-adwaita の CSD が有効になっている。プログラム的証拠で確認済み**
(スクリーンショットは事故で1枚破棄したため使っていない、§7)。

### 2.1 なぜ CSD になるか(コード上の裏付け)

winit の wayland バックエンド(`platform_impl/linux/wayland/window/state.rs`)は
`WinitFrame = sctk::shell::xdg::fallback_frame::FallbackFrame<WinitState>` を
装飾フレームとして使う。`wayland-csd-adwaita` フィーチャ(winit `0.30.13` の
**デフォルトフィーチャ**、`Cargo.toml` の `default = [..., "wayland-csd-adwaita"]`)
経由で `sctk-adwaita` がこの `FallbackFrame` の実装を提供する。GNOME/Mutter は
`zxdg_decoration_manager_v1` によるサーバーサイド装飾要求を一貫して拒否する
(これが Horizon が winit バックエンドを欲しがっている理由そのもの)ため、
`with_decorations(true)` で作った `WindowAttributes` は実質的に必ず
sctk-adwaita の CSD へフォールバックする。fork や追加設定は一切不要 ──
winit のデフォルトフィーチャに元から入っている。

### 2.2 実測ログ

`spikes/gpui-winit/src/window.rs` の `WinitPlatformWindow::new` に恒久的な
診断ログを仕込んである(`window.is_decorated()`/`inner_size()`/`outer_size()`):

```
window decoration evidence: is_decorated=true inner_size=PhysicalSize { width: 960, height: 540 } outer_size=PhysicalSize { width: 960, height: 593 }
```

`outer_size.height - inner_size.height = 53px` ── これが sctk-adwaita の
タイトルバー分。`inner_size`(コンテンツ領域)は要求した論理サイズ 640×360 ×
DPI スケールと一致し、`outer_size` だけがタイトルバー分大きい。これは
CSD がクライアント側で実際にレンダリングされ、winit がその分を
`outer_size` に計上している一次証拠になる(SSD であれば compositor 側の
装飾なので winit の `outer_size` は `inner_size` と一致するはず)。

## 3. 基準 b ── `Application::run` がパニックせず走る

複数回、3〜8秒間 `RUST_LOG=info`/`debug`/`trace` で起動 → 正常終了(`SIGTERM`
または `timeout`)まで一度もパニックなし。`cargo build`/`clippy` もクリーン
(§8)。CPU 使用率は再描画ループ稼働中で 10〜17%(1コア相当、後述 §4.2 の
vsync ペーシングが効いている証拠)── ビジーループで張り付いてはいない。

## 4. 基準 c ── gpui view が gpui_wgpu 経由で描画される

`main.rs` の `DemoView` は `div().track_focus(...).on_key_down(...)` の上に
テキストを1つ乗せただけの最小 view。`RUST_LOG=debug` でのログから:

```
cosmic_text::font::system: Parsed 4320 font faces in 221ms.
cosmic_text::font::system: font matches for Attrs { ... family: Name("Adwaita Sans") ... } in 29.543017ms
```

gpui のデフォルト UI フォント("Adwaita Sans")が cosmic-text 経由で実際に
解決・シェイピングされている ── レンダラーが空文字列を描いているのではなく、
グリフ形状が実際に生成されていることの証拠。`WgpuRenderer::draw(scene)` は
`RedrawRequested` のたびに例外なく呼ばれ続けた(§4.2)。

### 4.1 実装の要点

`gpui_wgpu::WgpuRenderer::new<W>` は
`W: HasWindowHandle + HasDisplayHandle + Debug + Send + Sync + Clone + 'static`
を要求する。`Arc<winit::window::Window>` はこれを無改造で満たす
(`winit::window::Window` 自体が rwh 0.6 の `HasWindowHandle`/`HasDisplayHandle`
を実装し、`raw-window-handle` 0.6 の `Arc<T: HasWindowHandle>` ブランケット実装が
残りを埋める)。gpui_linux が wayland サーフェスの生ポインタから手組みする
`RawWindow` ラッパーは不要だった ── winit を使う最大の実利の一つ。

### 4.2 再描画ループ

`Window::new`(gpui/src/window.rs)が登録する `on_request_frame` クロージャは、
呼ばれるたびに実際の draw + プレゼンテーションを行い、非フォーカス時は
~30fps、フォーカス時はスロットルなしという頻度制御を**内部に持っている**
(`RequestFrameOptions.require_presentation` を見て判定)。つまり
プラットフォーム側がすべきことは「いつ描画機会を与えるか」だけ。
gpui_linux は wayland の `frame` コールバック(compositor 主導の vsync 通知)
でこれを駆動するが、winit にはその等価物がない。本スパイクでは
gpui_web の requestAnimationFrame ループと同じ形 ── `WindowEvent::RedrawRequested`
のハンドラ内で `on_request_frame` コールバックを呼んだ直後に
`window.request_redraw()` を再度呼んで次のフレームを予約する自己再帰ループ
── を採用した(`app_handler.rs`)。実際のペーシングは wgpu サーフェスの
Fifo プレゼントモード(`preferred_present_mode: None` → デフォルト Fifo)の
`present()` 内 vsync ブロックに委ねている ── §3 の CPU 使用率(10〜17%、
張り付かない)が実測でこれを裏付ける。

## 5. 構造的知見(このスパイクの一番の要点)── `ActiveEventLoop` の可到達性

gpui の `Platform` トレイトは「window system への接続はいつでも持って
呼び出せる値」という前提で設計されている(X11 `Connection`、wayland の
globals、macOS の `Rc<dyn Platform>` 経由の Cocoa オブジェクト、ブラウザの
`web_sys::Window` ── いずれも `Platform` 実装が保持し続けられる)。
一方 winit 0.30 の `ActiveEventLoop` は **`ApplicationHandler` のコールバック
引数としてしか手に入らない** ── Android のようにイベントループが
サスペンド/リビルドされうるプラットフォームでも安全であるための、意図的な設計。

`Platform::open_window(&self, ...)` は gpui 内部の同期呼び出し連鎖の
どこからでも呼ばれうる(典型的には `on_finish_launching` クロージャの中、
つまり `resumed()` の中)。この2つを橋渡しする必要があり、
`spikes/gpui-winit/src/active_loop.rs` で次の方式を取った:

- `ApplicationHandler` の各コールバック(`resumed`/`window_event`/`user_event`/
  `about_to_wait`)の入口で、受け取った `&ActiveEventLoop` を
  `thread_local!` の生ポインタに退避する(`ActiveLoopGuard::enter`)。
  スコープを抜けるときに `Drop` でクリアする。
- `Platform::open_window` は `with_active_loop(|event_loop| ...)` で
  このポインタを取り出して `event_loop.create_window(attrs)` を呼ぶ。
  ガードが張られていない状態(= winit のコールバックの外)で呼ばれたら
  `None` を返し、`open_window` はエラーを返す(パニックしない)。

これは unsafe だが健全性の根拠は単純: 全部同一スレッド(winit のイベント
ループスレッド)で完結し、ポインタが指す借用は生成したコールバックの
スタックフレームより長生きしない(ガードの `Drop` がコールバック return
より必ず先に走る)。**実際に動いた** ── leg 1 の唯一のウィンドウは
`resumed()` の中の `on_finish_launching()` 呼び出しから開かれ、問題なく
`ActiveEventLoop` を取得できた。

複数ウィンドウや、`about_to_wait` 以外の非同期タイミング(例えばバック
グラウンドタスクの継続がメインスレッドにディスパッチされ、そこで新規
ウィンドウを開こうとするケース)でも、`dispatch_on_main_thread` で
キューされた仕事は必ず `user_event`/`about_to_wait` コールバックの中で
実行される(§6.1)ので、同じガードパターンでカバーされる ──
今回の smoke test では単一ウィンドウしか開いていないため、これは
コードレビューでの確認であり実測はしていない。

**stop-loss との関係**: これは「壁」ではなく「回避できた設計不一致」
だった。unsafe な thread-local ポインタ退避という迂回策で1回で解決し、
別の API を探す/複数ビルドサイクルを要する事態にはならなかった。

## 6. 基準 d ── キー入力が gpui に届く

### 6.1 ディスパッチャ/エグゼキュータの統合(タスクが「最難関」と予告していた箇所)

`gpui_linux::LinuxDispatcher` は calloop の `ping` ソースでバックグラウンド
スレッドからメインスレッドを起こす。winit の等価物は
`EventLoopProxy::send_event`(任意スレッドから呼べる、と winit のドキュメント
に明記されている)── `ApplicationHandler::user_event` を叩き起こす。
`spikes/gpui-winit/src/dispatcher.rs` の `WinitDispatcher` はこの1対1対応
そのままに実装した:

- `dispatch_on_main_thread`: gpui の `PriorityQueueSender`(calloop 版と
  同じプリミティブ、`gpui::queue` が web/linux 両バックエンドで共有している
  もの)にキューし、`proxy.send_event(WinitUserEvent::Wake)`。
- `WinitAppHandler::user_event`/`about_to_wait`/`resumed`/`window_event` の
  すべての入口で `dispatcher.drain_main_queue()` を呼ぶ。

**予告通りの箇所ではあったが、実装は驚きなく完了した** ── タスクブリーフが
示唆した通りのマッピングがそのまま機能した。むしろ §5 の `ActiveEventLoop`
可到達性のほうが実装上手間取った箇所だった。

### 6.2 キーマッピングと観測されたイベント

`app_handler.rs` の `winit_key_event_to_keystroke` は最小限のマッピング
(印字可能文字 + Space/Enter/Backspace/Escape/Tab)を `winit::event::KeyEvent`
→ `gpui::Keystroke` に変換し、`WindowEvent::KeyboardInput` を
`PlatformInput::KeyDown`/`KeyUp` として `on_input` コールバックへ渡す。
`main.rs` の `DemoView` は `div().track_focus(&handle).on_key_down(...)` で
これを受け、状態(`typed: String`)を更新して `cx.notify()`。

実行ログで実際に確認:

```
[...] gpui_winit_spike: typed buffer now: "e"
[...] gpui_winit_spike: typed buffer now: "em"
```

これは共有デスクトップ上でオーガニックに(こちらから明示的にキー入力を
送らずに)発生したイベントで、`WindowEvent::KeyboardInput` → `Keystroke`
変換 → `on_key_down` ハンドラ → `cx.notify()` → 再描画、というパイプ
ライン全体が実際に動作したことを示す。決定論的な自動テストにはできて
いない(下記)。

### 6.3 副産物の発見: GNOME は仮想キーボードプロトコルを拒否する

決定論的なキー注入を試すため `wtype`(`zwp_virtual_keyboard_manager_v1`
プロトコル経由)を使おうとしたが:

```
$ wtype hello-winit
Compositor does not support the virtual keyboard protocol
```

Mutter はこのプロトコルを実装していない(セキュリティ上の既知の方針)。
`winit::event::KeyEvent` はフィールドの大半が `pub` だが
`platform_specific: pub(crate) platform_impl::KeyEventExtra` を持つため
winit の外からリテラル構築もできない ── つまり本スパイクの環境では
「本物の compositor イベント配送を経ない」決定論的なキー入力テストの
選択肢がなかった。**将来 winit バックエンドを CI/ヘッドレスで自動検証
したくなった場合、この制約(GNOME 実機では仮想キーボード注入が使えない)
は先に踏んでおく価値がある**。X11(`xdotool`)や `ydotool`(uinput 経由、
compositor 非依存)であれば迂回できる可能性がある(未検証)。

## 7. 検証時の事故と回復(作業記録)

`gnome-screenshot -f` によるフルスクリーンショットを1回撮ったところ、
本スパイクのウィンドウではなくオーナーの Chrome(Claude.ai のチャット
画面、個人利用中の内容)が写り込んだ ── 共有デスクトップでの既知リスク
そのもの。画像は**即座に削除**し(`rm -f`)、以降は本ドキュメント §2/§4/§6
に記載した非視覚的証拠(`is_decorated`/`inner_size`/`outer_size` ログ、
フォントシェイピングログ、CPU 使用率、キー入力ログ)に切り替えた。
スクリーンショットによる視覚確認は今回のドキュメントには一切含まれて
いない。

## 8. gpui `Platform` トレイトサーフェスの荷重測定

タスク指示: 「スタブにされたメソッドのうち実際に起動時/描画時/入力時に
呼ばれるのはどれか」を計測する。`spikes/gpui-winit/src/platform.rs` の
代表的なスタブ(app ライフサイクル/ディスプレイ/キーボード/クリップ
ボード系、計31メソッド)に `log::trace!` を仕込み、5秒間のスモークテスト
(ウィンドウを開いてしばらく走らせるだけ、明示的なユーザー操作なし)で
`RUST_LOG=gpui_winit_spike=trace` を取った。

**実際に呼ばれた(=起動シーケンスで load-bearing)**:

| メソッド | 回数(5秒) | 備考 |
|---|---|---|
| `primary_display` | 1 | 起動時のディスプレイ解決 |
| `thermal_state` | 1 | `on_request_frame` のフレームレート判定(§4.2)が参照 |
| `on_thermal_state_change` | 1 | コールバック登録(`Window::new`) |
| `on_keyboard_layout_change` | 1 | コールバック登録 |
| `on_will_open_app_menu` | 1 | コールバック登録 |
| `on_validate_app_menu_command` | 1 | コールバック登録 |
| `on_app_menu_action` | 1 | コールバック登録 |
| `keyboard_mapper` | 1 | キーバインド解決に使用 |
| `keyboard_layout` | 1 | キーバインド解決に使用 |
| `activate` | 1 | `main.rs` が明示的に `cx.activate(true)` を呼んでいる分(gpui 側の自発呼び出しではない) |
| `hide_cursor_until_mouse_moves` | 3(1秒間隔) | **予想外** ── マウスを一度も動かしていないのに周期的に呼ばれる。gpui 内部に何らかのアイドル監視タイマーがあると推測されるが未調査 |

**5秒間のスモークテストでは一度も呼ばれなかった**(= このプロトタイプの
範囲では no-op で足りた): `quit`、`hide`/`hide_other_apps`/`unhide_other_apps`、
`displays`、`active_window`(gpui 自身からは)、`window_appearance`、`open_url`、
`on_open_urls`、`set_menus`、`set_dock_menu`、`app_path`、
`path_for_auxiliary_executable`、`set_cursor_style`、`is_cursor_visible`、
`should_auto_hide_scrollbars`、`read_from_clipboard`/`write_to_clipboard`、
`read_from_primary`/`write_to_primary`。

**解釈上の注意**: 「呼ばれなかった」は「実装しなくてよい」ではなく
「単一ウィンドウを開いてテキストを表示し、キー入力を1つ受けるだけの
シナリオでは経路に乗らなかった」という意味。`set_cursor_style` が
一度も呼ばれていないのは、本スパイクがマウスイベントを一切
`PlatformInput` に変換していない(leg 1 のスコープ外、`app_handler.rs`
参照)ことの裏返りであり、マウス対応を足せば確実に呼ばれるようになる
はず。`PlatformWindow` 側の主要スタブ(`gpu_specs`/`sprite_atlas`/
`is_subpixel_rendering_supported` など)は毎フレームの描画経路で
暗黙に叩かれている(パニックなく描画が継続していることがその証拠)
が、個別のトレースは仕込んでいない。

## 9. leg 2 に向けて(このスパイクではやっていないこと)

タスク指示通り、IME/preedit・クリップボード・メニュー・マルチウィンドウ・
画面キャプチャ・ドラッグ&ドロップは触っていない。特に IME は
`update_ime_position`/`set_input_handler` が本スパイクでは実質 no-op
(`PlatformInputHandler` を保存するだけで一度も使わない)なので、
leg 2 は事実上ゼロから設計することになる。マウス入力(`MouseMove`/
`MouseDown`/`MouseUp`/`ScrollWheel` の `PlatformInput` への変換)も
未実装 ── leg 1 の成功基準に入っていなかったための意図的な省略だが、
実務投入前には埋める必要がある。

## 10. 再現方法

```sh
cd spikes/gpui-winit
cargo build            # 初回は gpui 依存ツリーのフルビルドで数分
RUST_LOG=info ./target/debug/gpui-winit-spike
```

ウィンドウが開き、フォーカスした状態でタイプすると中央のテキストに
反映される(§6)。`RUST_LOG=gpui_winit_spike=trace` で §8 のトレースが
出る。`RUST_LOG=debug` で cosmic-text のフォント解決ログ(§4)が出る。

## 11. ゲート

`spikes/` は root workspace のメンバーではない
(`spikes/gpui-winit/Cargo.toml` に空の `[workspace]` テーブル)ため、
root の gate には影響しない。実測で確認:

```sh
cd spikes/gpui-winit && cargo fmt && cargo clippy   # クリーン(型複雑度の warning 1件のみ、gpui_web と同型)
cd <repo root> && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace && cargo build --workspace
```

いずれも green(詳細は本スパイクのレビューリクエストの gate tail を参照)。
