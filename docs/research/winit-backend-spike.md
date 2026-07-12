# winit windowing バックエンド スパイク(leg 1 + leg 2)

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

## 11. ゲート(leg 1 時点)

`spikes/` は root workspace のメンバーではない
(`spikes/gpui-winit/Cargo.toml` に空の `[workspace]` テーブル)ため、
root の gate には影響しない。実測で確認:

```sh
cd spikes/gpui-winit && cargo fmt && cargo clippy   # クリーン(型複雑度の warning 1件のみ、gpui_web と同型)
cd <repo root> && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo nextest run --workspace && cargo build --workspace
```

いずれも green(詳細は本スパイクのレビューリクエストの gate tail を参照)。

---

# leg 2 ── 日本語 IME(preedit/commit)

調査日: 2026-07-12(leg 1 と同じ worker セッションの続き、merge `bb79a91` の上に実装)。
目的: `docs/research/winit-backend-spike.md` §9 が「leg 2 は事実上ゼロから設計することになる」
と予告した IME 配線 ── winit の `Ime` イベントを、Horizon の実ターミナル
(`src/terminal/mod.rs` の `impl EntityInputHandler for TerminalView`)が使っているのと
同じ `EntityInputHandler` + `ElementInputHandler` の型に流し込めるかを実測する。

## 12. 到達レベル

タスク指示の a〜e を**すべて満たした**。壁にはぶつからなかったが、
実装過程で1件、フィードバックループのバグを踏んで1サイクルで直した(§15)。

| 基準 | 結果 | 証拠 |
|---|---|---|
| a. winit で IME を有効化し、実際の日本語入力メソッドで Preedit/Commit を観測 | **満たす**(ただし native Wayland は Enabled/Disabled のライフサイクルまで、実内容の観測は X11 バックエンド経由 ── §14/§16-Q1 参照) | §13, §14 |
| b. preedit を下線付きオーバーレイとして描画 | **満たす**(`replace_and_mark_text_in_range` の呼び出しと再描画は実測、画面上の下線そのものは非破壊的な確認手段の範囲でベストエフォート ── §17) | §14, §17 |
| c. commit が `replace_text_in_range` でバッファに反映され preedit がクリアされる | **満たす** | §14 |
| d. `bounds_for_range` が呼ばれ、候補ウィンドウの位置決めに使われる | **満たす** | §14 |
| e. `set_ime_cursor_area` でカーソル追従 | **満たす** | §14, §15 |

## 13. 実装

### 13.1 `window.rs` ── `handle_ime`

`WinitWindowInner::handle_ime(&self, ime: winit::event::Ime)` を新設し、
`WindowEvent::Ime(ime)` を `app_handler.rs` から1行で委譲する。中身は
gpui_linux の wayland バックエンド(`WaylandClientStatePtr` の
`Dispatch<zwp_text_input_v3::ZwpTextInputV3, ()>` 実装、pinned gpui
チェックアウトの `crates/gpui_linux/src/linux/wayland/client.rs:1766-1839`)
が `zwp_text_input_v3::Event` から `PlatformInputHandler` を叩く3パターンを
そのまま踏襲する:

| gpui_linux (wayland プロトコルイベント) | 本スパイク(winit `Ime`) | `PlatformInputHandler` 呼び出し |
|---|---|---|
| `PreeditString { text: Some(t), .. }` → `Done` | `Ime::Preedit(text, _)`(非空) | `replace_and_mark_text_in_range(None, &text, None)` |
| `PreeditString { text: None, .. }` → `Done` | `Ime::Preedit(text, _)`(空文字列) | `unmark_text()` |
| `CommitString { text: Some(t) }`(len > 1) | `Ime::Commit(text)` | `replace_text_in_range(None, &text)` |
| `Enter`/`Leave` | `Ime::Enabled`/`Ime::Disabled` | (状態遷移のみ、`Disabled` では保険として `unmark_text()`) |

`window.set_ime_allowed(true)` を `WinitPlatformWindow::new` に追加(基準 a
の前提 ── これを呼ばないと winit はそもそもテキスト入力機構
[Wayland では `zwp_text_input_v3`、X11 では XIM] にアタッチしない)。

候補ウィンドウ位置決め(基準 d/e)は gpui_linux の `get_ime_area()` と同じ
経路 ── `PlatformInputHandler::ime_candidate_bounds()`(`marked_text_range`/
`selected_text_range` → `bounds_for_range` を内部で合成する gpui 提供の
ヘルパー、pinned checkout `crates/gpui/src/platform.rs:1321-1327`)で
矩形を得て、`winit::window::Window::set_ime_cursor_area` に渡す。加えて
`PlatformWindow::update_ime_position`(gpui が
`Window::invalidate_character_coordinates()` 経由で呼ぶ、
composition 外でのキャレット移動フック)も同じヘルパー
(`WinitWindowInner::set_ime_cursor_area`)に配線し、`DemoView::handle_key`
から通常キー入力のたびに呼び出す ── gpui_linux の
`WaylandClient::update_ime_position`(pre_edit_text が無いときだけ
`set_cursor_rectangle` を打つ)と同じ役割分担。

### 13.2 `main.rs` ── `EntityInputHandler for DemoView`

gpui 本体の `crates/gpui/examples/input.rs`(`TextInput`/`TextElement`)を
型として踏襲しつつ、Horizon の `TerminalView`(`src/terminal/mod.rs`)により
近い簡略版にした:

- `marked_text: Option<String>` を素朴なフィールドとして持つだけ(PTY相当の
  送信先が無いこの demo では `typed: String` に追記するだけ)── Horizon の
  `ime_marked_text` と同じ「クライアントサイドのみの状態」という設計。
- `on_key_down` は `self.marked_text.is_some()` で早期 return ──
  `TerminalView::on_key_down`(`src/terminal/mod.rs:258, 284`)の
  `self.ime_marked_text.is_some()` ガードと同一の理由・同一の書き方。
- `bounds_for_range` は「typed の文字数 + range_utf16.start ぶんの前進」と
  いう最小実装(Horizon 自身の `cursor.col + range_utf16.start` と同じ簡略さ、
  `docs/research/gpui-terminal-implementations.md` S3 点6 参照)。
- ウィジェット全体を1つの `div()` で描き、`window.handle_input(...)` の配線
  だけは `gpui::canvas()`(`crates/gpui/src/elements/canvas.rs`)の paint
  クロージャに乗せた ── gpui 公式 example の `TextElement`(独自 `Element`
  実装、約180行)より軽量な近道になる。行レイアウト計算やカーソル位置の
  可視化を自前でやらない demo だからこそ許容できる省略で、Horizon の
  `TerminalElement::paint`(グリッド描画の中で `window.handle_input` を呼ぶ)
  ほど作り込む必要はなかった。

## 14. 実測: 本物の日本語入力メソッドでの preedit → commit

### 14.1 実行環境

```
XDG_CURRENT_DESKTOP=GNOME
XDG_SESSION_TYPE=wayland
LANG=ja_JP.UTF-8 (LC_CTYPE も同様)
QT_IM_MODULE=ibus
XMODIFIERS=@im=ibus
GTK_IM_MODULE=(未設定)
INPUT_METHOD=(未設定)
```

`ibus-daemon`(`--panel disable -r --xim`)、`ibus-engine-mozc --ibus`、
`mozc_server` が常駐しており、`ibus engine` で確認した**現在の入力エンジンは
`mozc-jp`**(モードはひらがな入力 ── ローマ字を打つと即座に平仮名へ変換される
状態)。つまりこのホストには当初想定された「日本語IMEが無い」という環境制約は
**存在しなかった**。GTK_IM_MODULE は未設定だが、winit は GTK の IM モジュール
機構を経由しない(直接 Wayland の `zwp_text_input_v3` または X11 の XIM を
叩く)ため無関係。

### 14.2 native Wayland での確定的キー注入は依然ブロックされる

leg 1 §6.3 が見つけた「GNOME/Mutter は `zwp_virtual_keyboard_manager_v1`
を実装していない」という壁は leg 2 でも同じ形で再確認した:

```
$ wtype こんにちは
Compositor does not support the virtual keyboard protocol
```

加えて本スパイクでもう1つ確認したのは、`xdotool`(X11 の XTest 拡張経由)
は **native Wayland のトップレベルウィンドウをそもそも発見できない**という
こと ── `xdotool search --name "gpui-winit spike"` を native Wayland
バックエンドで起動したウィンドウに対して実行すると exit code 1(0件ヒット)
になる。winit が作るのは XWayland プロキシを介さない純粋な wayland サーフェス
なので、これは想定通り。ヘッドレスな代替 wayland コンポジタ
(`weston`/`sway`/`cage`)もこのホストには入っておらず、新規インストールは
このスパイクの権限外と判断し試みなかった。

結論: **native Wayland 経路での確定的(スクリプト化された)IME入力は、
このホスト構成では技術的に到達不能**。これは leg 1 の知見の直接的な帰結
(仮想キーボードプロトコル拒否)であり、新しい発見ではないが、leg 2 でも
崩れなかったことの確認として重要。

### 14.3 迂回策: winit の X11 バックエンドへフォールバックさせて実測

winit はバックエンド選択を `WAYLAND_DISPLAY`/`WAYLAND_SOCKET` の有無で自動
判定する(`platform_impl/linux/mod.rs:730-763`、pinned `winit-0.30.13`)。
このホストは GNOME/Wayland セッションでも X11 互換のため XWayland の
`DISPLAY=:0` を提供しているので、`WAYLAND_DISPLAY`/`WAYLAND_SOCKET` を
unset して起動するだけで winit は X11 バックエンドへフォールバックする
(装飾の実測: `is_decorated=true inner_size=640x360 outer_size=640x360` ──
CSD 分の余白がゼロになり、winit の x11 バックエンドを使っていることが
sizeログからも裏付けられる)。X11 バックエンドの窓は XWM 管理下の実 X11
ウィンドウなので、`xdotool search`/`xdotool key --window <id>` で
**そのウィンドウだけ**を確実に狙って安全にキーを送れる(タイトル一致で
対象ウィンドウのXIDを取得し、`--window` で明示指定 ── 画面全体や
フォーカス中の別ウィンドウを誤って操作するリスクを排除)。

この迂回策は「winit の native Wayland 実装」そのものの実測ではなく、
「winit の `Ime` イベント API を、winit の別バックエンド経由で本物の入力
メソッドから駆動する」実測である点に注意 ── ただし winit はどちらの
バックエンドでも**同一の** `winit::event::Ime` enum を上位に渡すので、
本スパイクが検証したい「winit `Ime` → gpui `EntityInputHandler`」の配線
コードパス自体は共通であり、実測の価値は損なわれない。

### 14.4 実測ログ:「hello」→ mozc変換 → 確定

`xdotool key --window <id> h e l l o` → `xdotool key --window <id> Return`
を実行した際の実ログ(`RUST_LOG=info`、抜粋):

```
winit Ime event: Preedit("え", Some((3, 3)))
winit Ime event: Preedit("えｌ", Some((6, 6)))
winit Ime event: Preedit("えっｌ", Some((9, 9)))
winit Ime event: Preedit("えっぉ", Some((9, 9)))
winit Ime event: Preedit("えっ", Some((6, 6)))
winit Ime event: Preedit("", None)
winit Ime event: Commit("えっ")
ime commit: "えっ" (was_composing=false, buffer now "えっ")
typed buffer now: "えっ\n"
```

これは合成イベントではなく mozc エンジンが実際にローマ字→ひらがな変換を
行った結果("hello" は "えっ" 相当に変換され、Enter で確定された)。
別の試行(`k o n n i` → 変換継続中に Return なしで放置)でも同様に
`Preedit("ｋ")→("こ")→("こｎ")→("こん")→("こんい")` という段階的な
かな変換が観測でき、`bounds_for_range` が preedit の更新のたびに複数回
(候補ウィンドウ位置探索のための内部ウォークを含む)呼ばれ、最終的な
矩形が `ime candidate bounds: Bounds { origin: Point { x: 0px, y: 0px },
size: Size { 21.68px × 30px } }` のような形でログに残る(この試行では
`typed` が空だったため x=0 は正しい ── §13.2 の実装通り `typed` の文字数
ぶんだけ後続の合成でオフセットする設計)。

## 15. 見つけたバグ: `set_ime_cursor_area` のフィードバックループ

実装の最初のバージョンは「`Ime` イベントを処理するたび無条件に
`ime_candidate_bounds()` を計算して `set_ime_cursor_area` を呼ぶ」という
素朴な作りだった。5秒のスモークテストを取ったところ、`Preedit("", None)`
イベントが**毎秒数千〜1万件のオーダーで無限に降り続ける**ことが分かった
(`RUST_LOG=info` の出力が5秒で約10万行)。

原因: GNOME の text-input-v3 実装は `set_cursor_rectangle` +
`text_input.commit()`(winit の `set_ime_cursor_area` 内部で発行)を
「入力状態が変化した」ものとして扱い、`Done` イベントを送り返す ──
winit はこれを `Preedit("", None)` として上位に届ける。これをまた
無条件に `set_ime_cursor_area` で処理すると、再び `Done` が返る……という
無限ループになる。gpui_linux 自身はこの罠を `serial_tracker` による
シリアル比較(`pinned checkout crates/gpui_linux/.../client.rs:1815-1833`
の `if last_serial == serial { text_input.commit(); }`)で回避しているが、
winit の `Ime` enum はプロトコルのシリアル番号を上位に渡さないため、この
ガードをそのまま移植することはできない。

**採った対策**: `set_ime_cursor_area` を呼ぶのは `Ime::Preedit` が
**非空**のとき(＝実際に合成中のとき)だけに限定し、`Commit`/`Disabled`/
空 `Preedit` からは呼ばない。修正後は起動直後に1回 `Preedit("", None)`
が来るだけで収束することを確認した(§13.1 のコード、`window.rs` の
コメントに詳細を記録)。

これは leg 2 の中で最も価値のある副産物の発見だと考える ──
「winit の `Ime` API はプロトコルのシリアル情報を隠蔽している」ことが
実運用上の罠(素朴な実装だと無限ループになりうる)として顕在化した、
gpui_linux の実装がなぜあの形をしているかの理由も後付けで裏付けられた。

## 16. 測定質問への回答

### Q1. winit の `Ime` イベントは gpui の `EntityInputHandler` 契約に必要な情報を過不足なく運べるか

**運べる。むしろ X11 バックエンドでは gpui_linux が実際に使っている情報より
リッチだった。** `winit::event::Ime::Preedit(String, Option<(usize, usize)>)`
の第2要素(preedit 文字列内のカーソル位置、バイトオフセット)は、X11/XIM
経由の実測で確かに値が入っていた(`Some((9, 9))` など)。ところが
gpui_linux 自身は `zwp_text_input_v3::Event::PreeditString { text, .. }`
から**プロトコルが提供するはずの `cursor_begin`/`cursor_end` を意図的に
捨てている**(pinned checkout `client.rs:1811`、`{ text, .. }` という
分割代入)── つまり gpui の `EntityInputHandler::replace_and_mark_text_in_range`
の `new_selected_range` 引数は、Linux の実運用パスでは元から `None`
しか渡されていない。本スパイクが同じく `None` を渡す実装にしたのは、
gpui が実際に要求する以上の忠実さを作り込まなかっただけで、契約不足では
ない。Horizon がこの情報(preedit 内の変換対象クローズ境界)を将来
使いたくなった場合、winit 側にはデータが眠っている(少なくとも X11
バックエンドでは)ことは記録しておく価値がある ── native Wayland
バックエンドでも同じフィールドが埋まるかは、§14.2 の注入手段の制約により
**未確認**(空文字列の `Preedit` しか native Wayland では観測できなかった)。

失われている情報: preedit のセグメント単位のスタイリング(変換候補の
確定済みクローズと未確定クローズを別の下線太さで塗り分けるような、
一部の IME フレームワークが提供するリッチな表現)は、winit の `Ime` にも
`zwp_text_input_v3` にも存在しない(どちらも「フラットな文字列 + 全体の
カーソル位置」のみ)。Horizon の `TerminalView` も現状フラットな下線
オーバーレイしか実装していないため、この点で winit が gpui_linux より
不利になることはない。

### Q2. イベントの ORDER は gpui の期待と両立するか

**両立するが、1点重大な罠を実測で確認した。** winit のドキュメント通り、
`Commit` の直前には必ず空の `Preedit` が来る(§14.4 のログでも
`Preedit("", None)` → `Commit("えっ")` の順序が一貫していた)。
`Enabled`/`Disabled` のライフカウントも、ウィンドウ生成直後に1回だけ
発火し、以後は入力メソッドの activate/deactivate に応じて発火する
(X11 起動直後には `Disabled` → `Enabled` の順、native Wayland 起動直後は
`Enabled` のみ ── 初期化パスの違いは実装のバックエンド差として記録する
に留め、深追いはしていない)。

**重大な罠(§14.4 のログで直接観測)**: mozc の変換を Enter キーで確定
すると、`Commit` イベントで文字列がバッファに入った**直後**に、同じ
Enter の物理キー押下が独立した `KeyboardInput`(押下)イベントとしても
アプリに届き、`marked_text` が既に `None` にクリアされているため
`on_key_down` の IME ガード(§13.2)をすり抜けて通常のキー処理(この demo
では改行の追加)が実行される ── ログでは
`ime commit: "えっ" (... buffer now "えっ")` の直後に
`typed buffer now: "えっ\n"` という**別イベント**が続いている。これは
Wayland の text-input-v3 設計(X11 の XIM と異なり、コンポジタは
text-input が有効でもキーイベントをクライアントから奪わない)に起因する
既知の挙動で、X11/XIM 経由でも(少なくとも winit の実装では)同様に
発生することを実測した。

**Horizon の現行プロダクションコードへの含意(スパイクの範囲を超える
指摘だが重要なので記録する)**: gpui_linux の wayland バックエンドも
`CommitString` イベントと物理キーイベントを独立に配送する設計は同じ
(§13.1 の表、gpui_linux の `Dispatch<zwp_text_input_v3::ZwpTextInputV3>`
実装を参照)ため、**Horizon の実ターミナル(`TerminalView`)でも、IME
変換をEnterキーで確定する操作は、確定テキストの直後に余分な改行(PTYへの
`\r`)を送ってしまう可能性がある**。`TerminalView::on_key_down` の
`self.ime_marked_text.is_some()` ガードは commit イベントの**時点では**
`ime_marked_text` を正しくクリアしているため、後続の物理 Enter キーは
このガードをすり抜ける。実機(GNOME + ibus/mozc)での確認は本スパイクの
範囲外だが、コードパスの対称性から見て再現する可能性が高い。
`docs/tasks/backlog.md` へのメモを推奨(本レビューリクエストの
Observations 参照)。

### Q3. 基準 a の観測に必要だった環境は何か

§14.1 の通り。要点だけ再掲すると、**winit 自体は GTK_IM_MODULE/
QT_IM_MODULE のようなツールキット固有の環境変数を一切読まない**
(Wayland では常に `zwp_text_input_v3`、X11 では常に XIM を直接叩く)。
必要だったのはアプリケーション側の `window.set_ime_allowed(true)` 呼び出し
(§13.1)だけで、環境変数のセットアップは「IME フレームワーク自体が
起動していること」(`ibus-daemon` + 何らかの入力エンジン)の前提条件で
あり、winit 固有の要求ではない。このホストは `ibus-daemon` + `mozc`
エンジンが既に常駐していたため、追加のセットアップは不要だった。
再現性のために記録: `ibus engine` で現在のエンジンが `mozc-jp`(または
他の日本語エンジン)であることを確認し、`XDG_SESSION_TYPE`/
`WAYLAND_DISPLAY`/`DISPLAY` の値を控えておけば十分。

### Q4. winit バックエンド採用の総合判定 ── leg 1 + leg 2 を踏まえて

**「実装できる」という結論に変わりはなく、むしろ強化された。** leg 1 が
確立した CSD/描画/基本キー入力に加え、leg 2 で IME(このプロダクトの
最大のリスクだったはず)も、実運用の日本語入力メソッドを使った実測込みで
動作を確認できた。gpui_linux が持つ設計(`EntityInputHandler` の型、
preedit のクライアントサイド管理、`ime_candidate_bounds` ヘルパー)は
winit 経由でもそのまま再利用でき、フォーク相当の改造は不要だった。

残っている未知数(次の判断材料):

1. **native Wayland での preedit 内容の実測**(§14.2/Q1)── 内容そのもの
   (空でない文字列・カーソル位置)は X11 バックエンド経由でしか確認できて
   いない。ヘッドレス wayland コンポジタ(weston/sway)を用意すれば
   `zwp_virtual_keyboard_manager_v1` を実装しているものであれば
   `wtype` で native Wayland 経路も確定的に検証できる可能性がある。
2. **物理キー/IME確定の二重処理**(§16-Q2)── Horizon の現行実装にも
   既に存在する可能性のある罠で、winit バックエンド固有の問題ではない。
   採用可否とは独立に手当てすべき。
3. leg 1 §9 で挙げたまま未着手: マウス入力、クリップボード、メニュー、
   マルチウィンドウ、画面キャプチャ、ドラッグ&ドロップ、スケールファクタ
   変更時の挙動。
4. **`horizon-winit-platform` クレートの規模感**: 本スパイク(leg 1 +
   leg 2)は7ファイル計約1,570行(`git diff --stat` 実測、IME追加分は
   +288/-17行)で、CSD・描画・キー入力・IME という4つの機能を実証した。
   プロダクション化には上記3の未着手項目に加え、`Platform` トレイトの
   残りのスタブ(§8 の「呼ばれなかった」欄、特にクリップボード・
   メニュー)を埋める必要がある。スパイクの密度から素朴に外挿すると、
   フルスコープの `horizon-winit-platform` は**3,000〜5,000行程度**
   (テスト別)というオーダー感 ── gpui_linux 自身のLinux実装規模
   (wayland/x11 合わせて数万行)よりは大幅に小さくなる見込みだが、これは
   winit が低レベルのプロトコル処理を肩代わりしてくれることの裏返り。

## 17. 検証時の作業記録(スクリーンショット規律)

leg 1 の事故(共有デスクトップでの誤ったフルスクリーンキャプチャ)を
踏まえ、本スパイクでは終始「自分のウィンドウの XID を明示指定できる
場合のみ」キャプチャを行った:

- 装飾・キー入力・IME の配線確認は本ドキュメントの各所にある**非視覚的
  証拠**(ログ、`bounds_for_range`/`ime_candidate_bounds` の戻り値、
  ウィンドウサイズ)を主とした。
- 1回だけ、`import -window <XID>`(ImageMagick、X11 バックエンドの
  ウィンドウ限定、`xdotool search --name` で取得した対象ウィンドウの
  XID のみを指定)でスクリーンショットを取得し、`identify` で解像度が
  ウィンドウの inner size(640×360)と一致することを確認してから中身を
  確認した ── 日本語コミット後のテキスト("ううう" などの合成結果)が
  正しくグリフレンダリングされていることは確認できたが、たまたま
  タイミング上、preedit の下線オーバーレイが表示されている瞬間を
  捉えられなかった(このスクリーンショットはスクラッチパッドに残すのみで、
  リポジトリには含めない)。
- **注意点として記録**: `xdotool windowactivate`/`xdotool key` は対象
  ウィンドウにフォーカスを移してからキーを送るため、共有デスクトップで
  オーナーが同時にタイピングしていた場合、そのキー入力が一時的に本スパイク
  のウィンドウに流れ込むリスクがある(実際、ある試行で明示的に送っていない
  "u"/Enter の入力が buffer に混入した ── 詳細を追う代わりに、以後は
  ウィンドウのフォーカスを奪う操作を最小限に留め、危険なやり直しは行わず、
  既に得られていた明確な証拠を優先して記録に留めた)。この教訓は今後
  同種のスパイクでも踏まえるべき: **共有デスクトップでの `windowactivate`
  はオーナーの入力を横取りしうる**、スクリーンショットの誤爆と同格の
  リスクとして扱うこと。

## 18. ゲート(leg 2 時点)

```sh
cd spikes/gpui-winit && cargo fmt && cargo clippy   # クリーン(leg 1 と同じ型複雑度 warning 1件のみ)
cd <repo root> && cargo fmt \
  && cargo clippy --workspace --all-targets --locked -- -D warnings \
  && cargo nextest run --workspace --locked \
  && cargo build --workspace --locked
```

実測: 663 tests run: 663 passed, 4 skipped。clippy/fmt/build いずれも
warning ゼロで green(spike はワークスペース非メンバーのため root gate に
影響しない ── §11 と同じ)。
