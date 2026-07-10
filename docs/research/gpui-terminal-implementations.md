# GPUI ベース ターミナルエミュレータ実装調査レポート

調査日: 2026-07-10(worker セッションによる調査、`docs/gpui-migration-consideration.md` のスパイク S1–S3 の設計参照用)。調査対象: Zed (`terminal`/`terminal_view`, GPL — 参照専用), `zortax/gpui-terminal`, `Xuanwo/gpui-ghostty`, `nowledge-co/con-terminal`, `lassejlv/termy`。GitHub API・`git clone`・`rg` によるソース直読で確認した内容のみを記載する(推測は明示する)。

---

## 1. サマリー表

| プロジェクト | エミュレーションバックエンド | GPUI IME 対応 | ライセンス | 核となるビュー層の規模(概算) | 位置づけ |
|---|---|---|---|---|---|
| **Zed** `crates/terminal` + `crates/terminal_view` | 自前フォーク `zed-industries/alacritty`(`alacritty_terminal 0.26.1-dev`、upstream から5コミット先行) | ○(`InputHandler` を手動実装するアダプタ構造体経由) | **GPL-3.0-or-later**(読解専用、コピー不可) | 約 12,000〜19,000 行(本体6,777行 + element/view 5,578行 + panel/persistence/hyperlink等アプリ統合 7,000行超) | 唯一の「実運用で数年鍛えられた」実装。IME・damage・マウス報告の設計思想の参照元として最重要だが、コード転用不可 |
| **zortax/gpui-terminal** | 公開crateの `alacritty_terminal = "0.25.1"` | **✕(IME未実装)** | **MIT OR Apache-2.0** | 約 6,031 行(クレート全体) | 最小構成の単体crate。ライセンス的には最も自由だがIMEの答えにならない |
| **Xuanwo/gpui-ghostty** (`gpui_ghostty_terminal`) | **Ghostty のVTコア**をFFI経由で利用(alacritty不使用)。行単位のdirty管理はGhostty本体由来 | **○(`EntityInputHandler` + `ElementInputHandler` 王道パターン)** | **Apache-2.0** | 約 3,171 行(`gpui_ghostty_terminal` crate) | IME配線の教科書的な最小実装として最良の参照。ただしバックエンドがalacritty系ではない |
| **nowledge-co/con-terminal** (`con-ghostty` + `con-app`) | プラットフォーム毎に別物: macOSは**libghostty本体をネイティブNSView埋め込み**(GPUI描画を経由しない)、Windowsは独自VT+D3D11パイプラインをGPUI画像合成で表示、Linuxは自前の小型VTパーサ (`vt.rs`) を使う**WIPスキャフォールド** | **○(Linux限定、`TerminalImeView` trait + ジェネリック `TerminalImeInputHandler<V>` という再利用可能な抽象)** | **MIT** | Linux GPUI描画パス `con-app/src/linux_view.rs` 2,033行 + `terminal_ime.rs` 171行。macOS/Windows含む全体は13,752行(con-ghostty)+ α | 「GPUI Element で描く」戦略以外の代替案(ネイティブView埋め込み/テクスチャ合成)を実例で示す点で示唆に富むが、Horizonの狙う経路(alacritty→GPUI Element)の直接参照にはならない |
| **lassejlv/termy** (`crates/desktop_app/src/terminal_view` + `crates/terminal_ui`) | `alacritty_terminal = "0.26"`(**horizon-terminal-coreと同一マイナーバージョン**)、`gpui` はZedリポジトリからgit依存 | **○(`EntityInputHandler` + `ElementInputHandler`、preeditオーバーレイ実装あり)** | **MIT** | 核となるレンダリング/入力/IME部分だけで約 13,000 行超(`grid.rs` 3,834 + `render.rs` 4,380 + `inline_input.rs` 1,730 + `interaction/input.rs` 1,045 + `interaction/mouse*.rs` 2,271)。周辺のタブ/コマンドパレット等を含む `terminal_view/` ディレクトリ全体は44,693行 | **アーキテクチャ的に最もHorizonの狙いに近い実運用実装**。alacritty_terminalのdamage APIを使った行/列単位のdirty tracking実装が確認できる唯一のプロジェクトで、**kitty keyboard protocolのモード対応エンコーダも完全実装**(§2.5の訂正注記) |

---

## 2. プロジェクト別詳細

### 2.1 Zed (`crates/terminal`, `crates/terminal_view`) — GPL、参照専用

> **ライセンス注意**: `crates/terminal/Cargo.toml:6` および `crates/terminal_view/Cargo.toml:6` はいずれも `license = "GPL-3.0-or-later"`。GPUI自体・gpui-component自体はApache-2.0だが、この2クレートはGPLである。読解して設計思想を学ぶのは問題ないが、**コード片のコピー&ペーストは不可**。

**バックエンド**: `Cargo.toml:513` で `alacritty_terminal = { git = "https://github.com/zed-industries/alacritty", rev = "4c12966..." }` と、独自フォークをgit依存で固定している。フォーク先の `alacritty_terminal/Cargo.toml` を見ると `version = "0.26.1-dev"` ── horizon-terminal-coreの0.26系とほぼ同世代であり、GridのAPI形状は近いと推測できる。GitHub比較 (`zed-industries/alacritty` vs `alacritty/alacritty` master) では ahead 5 / behind 1 commits と、パッチはごく小さい。

**グリッド→GPUIの橋渡し**: `crates/terminal/src/alacritty.rs:807` の `make_content()` が毎イベントで `term.renderable_content().display_iter` から可視領域全体を舐めて `Content { cells: Vec<IndexedCell>, .. }` を作り直す。**セル単位のdamage/dirty矩形は使っていない**――フルスナップショット方式。その代わりイベント側で間引く: `terminal.rs:1315` の `subscribe()` が「最初の1イベントは即処理→以後は4msタイマーで束ねて最大100件/バッチにcoalesce」という設計(コメント: "Process the first event immediately for lowered latency")。

**グリッド描画**: `crates/terminal_view/src/terminal_element.rs` が `impl Element for TerminalElement`(`prepaint`/`paint`)を実装するカスタムElement。同一スタイル(色・下線・打消し線)が連続するセルを `BatchedTextRun`(`terminal_element.rs:96`)にRLE的にまとめ、`window.text_system().shape_line()` を1バッチにつき1回呼ぶ(`terminal_element.rs:160-176`)。背景色も同様に `BackgroundRegion`/`merge_background_regions()`(`terminal_element.rs:221,294`)で矩形マージしてquad数を削減。カーソルは `CursorLayout`(editor crateの型を再利用)。

**IME(最重要)**: `TerminalInputHandler`(`terminal_element.rs:1511`)という**専用の小さな構造体**を作り、そこに `impl InputHandler for TerminalInputHandler`(`:1517`)を実装 ── Zedのこのコードは古めの `InputHandler`(Windowレベルのオブジェクト安全trait)を直接実装するスタイルで、後発の `EntityInputHandler` パターン(後述の3プロジェクトで採用)とは異なる。

- `marked_text_range`/`replace_and_mark_text_in_range`/`unmark_text` は全て `TerminalView`(`terminal_view.rs:64` の `struct ImeState { marked_text: String }`)へ委譲。
- `replace_text_in_range`(確定コミット)は `view.clear_marked_text()` → `view.commit_text()` の順で呼ぶ。`commit_text`(`terminal_view.rs:396`)は単純に `term.input(text.into_bytes())` ── **確定テキストはエスケープシーケンス生成を経由せず生バイトでPTYに書く**。
- preeditのオーバーレイ描画は `paint()` 内(`terminal_element.rs:1430-1472`)で、marked_textがあれば「下線付きTextRunをshapeし、まずカーソル位置に背景色quadを塗って裏の端末文字を隠してから前面に描く」。カーソル自体はmarked_text表示中は描かない(`:1474-1479` の `marked_text_cloned.is_none()` 条件)。
- `bounds_for_range`(IME候補ウィンドウ位置決め)はカーソル位置に `range_utf16.start * cell_width` を加算するだけの単純な実装(`:1592-1605`)。

**入力マッピング**: `mappings/keys.rs` はコメントで "created from reading the alacritty source" と明記。kitty keyboard protocolへの言及は**リポジトリ全体を検索してもゼロ件** ── Zedのターミナルはkitty protocol非対応。マウスは `mappings/mouse.rs` でSGR/UTF8/Normalの3モードをサポート(`MouseFormat::from_mode`)。

---

### 2.2 zortax/gpui-terminal — MIT/Apache、IME非対応

単体crate(`Cargo.toml`: `license = "MIT OR Apache-2.0"`、`alacritty_terminal = "0.25.1"`、`gpui = "0.2.2"`)。`src/render.rs` はZedと収束的に同じ設計(背景マージ・テキストランバッチ、ドキュメントコメントに明記: "1. Background Merging... 2. Text Batching...")。ただし `InputHandler`/`EntityInputHandler`/`marked_text` を全ファイルgrepしても**一致なし**。`src/input.rs` はキーストロークを直接エスケープシーケンスへ変換するだけ(kitty protocolも非対応)。**IMEの実装がまるごと欠落しており、S3の参照にはならない**。ライセンス上は最も自由(デュアルMIT/Apache)だが、この点で不採用。

---

### 2.3 Xuanwo/gpui-ghostty (`gpui_ghostty_terminal`) — Apache-2.0、IME配線の教科書

バックエンドはGhostty本体のVTコア(`vendor/ghostty`、Zig)をFFI経由 (`ghostty_vt_sys`/`ghostty_vt`) で利用。alacritty系ではないため文法バックエンドとしての直接参照価値はないが、**GPUI側のIME配線は最もクリーンで、そのまま設計の型として使える**。

- `crates/gpui_ghostty_terminal/src/view/mod.rs:1205` で `impl EntityInputHandler for TerminalView` ── ビューのEntity自身に直接実装する新しめの様式。
- `paint()`内(`view/mod.rs:1981-1985`)で `window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.view.clone()), cx)` と呼ぶだけで配線完了。`ElementInputHandler::new()` がGPUI側で用意されたアダプタで、`EntityInputHandler` を実装したEntityから `InputHandler` オブジェクトを自動生成してくれる。
- `tests.rs:361` に `is_ime_in_progress()`(GPUIの`Keystroke`が持つ組み込みメソッド)と `should_skip_key_down_for_ime()`(`view/mod.rs:35`)── **IME変換中はon_key_downでの直接キー処理をスキップする**、というガードが必須であることを示すテストが存在。ここはHorizon実装で見落としやすい罠。
- `crates/gpui_ghostty_terminal/src/session.rs:342` に `take_dirty_viewport_rows()` ── Ghostty側の行単位damage集合を取り出すAPI。行レベルの再描画最適化がバックエンド側から提供されている好例。

---

### 2.4 nowledge-co/con-terminal — MIT、プラットフォーム別に別戦略

`con-terminal`(薄いテーマ用glue、579行)+ `con-ghostty`(13,752行、libghostty FFI)+ `con-app`(アプリ本体)という構成。**最大の発見**は、`con-ghostty/src/lib.rs` 冒頭のバックエンド選択表:

| target | backend |
|---|---|
| macOS | libghostty本体(Metal + AppKit NSView)をそのまま埋め込み |
| Windows | libghostty-vt + ConPTY + D3D11/DirectWriteの自前パイプラインを「GPUI画像合成」で表示 |
| Linux | 自前の小型VTパーサ (`vt.rs`) + 将来的な「GPUI-owned renderer」(WIPスキャフォールド) |

つまり**メインのmacOSパスはそもそもGPUIのElement/paintを使っていない**――`terminal.rs` 内で `ghostty_surface_text` によるIME/composeパイプラインもAppKitのネイティブNSTextInputClient経路に委ねられており、GPUIのInputHandlerを経由しない。「GPUIでターミナルを描く」問題そのものを、ネイティブビュー埋め込みで回避する設計判断であり、Horizonが検証したい経路(alacritty grid → 自前GPUI Element)とは別解になる。

唯一Horizon的に価値があるのはLinuxパス(`con-app/src/linux_view.rs`, 2,033行、明示的に "future GPUI-owned renderer" とコメントされたWIP)で使われる `con-app/src/terminal_ime.rs`(171行)── ジェネリックな `TerminalImeView` traitと、それに対する単一の `impl<V: TerminalImeView> InputHandler for TerminalImeInputHandler<V>`(`terminal_ime.rs:54`)という**再利用可能なIME抽象**。バックエンド(Ghostty/alacritty問わず)に依存しない設計で、`prefers_ime_for_printable_keys()`(`:168`)という他プロジェクトでは見なかったフックも実装している。MITライセンスなので、このファイル単体は設計として(あるいは条件付きでコードとして)参照する価値が高い。

---

### 2.5 lassejlv/termy — MIT、**最有力の実装参照**

`Cargo.toml:25` に `alacritty_terminal = "0.26"` ── **horizon-terminal-coreと同一マイナーバージョン系列**。`gpui` はZedリポジトリからのgit依存(`rev = "83de8a25..."`固定)。MITライセンス(`LICENSE:1`)。

コア実装は2クレートに分かれる:

- `crates/terminal_ui`: バックエンド非依存のドメインロジック(tmux連携、grid、OSC intercept等)。特に `src/grid.rs`(3,834行)に `enum TerminalGridPaintDamage`(`grid.rs:36`)── コメントに **"Row damage with column bounds ... Emitted when alacritty reports partial damage with column-level granularity"** と明記されており、**alacritty_terminal自身が提供するdamage/dirty tracking APIを実際に使い、行単位どころか列範囲単位で再描画範囲を絞り込んでいる**。これは調査した5プロジェクト中、alacritty系バックエンドで damage APIを本格的に活用している唯一の例(Zedはフルスナップショット、zortaxもフルリビルド)。`dirty_rows: Vec<usize>` / `dirty_col_ranges: Vec<Option<(usize,usize)>>`(`grid.rs:437-441`)というスクラッチバッファを使い回して再描画対象のみ処理する。
- `crates/desktop_app/src/terminal_view`: GPUI Element層。`render.rs`(4,380行)と `inline_input.rs`(1,730行)にIME実装。
  - `inline_input.rs:1423` で `impl EntityInputHandler for TerminalView`(Xuanwo/gpui-ghosttyと同じ新様式)。
  - `inline_input.rs:890` および `render.rs:3180` で `ElementInputHandler::new(bounds, view.clone())` を経由して配線。
  - `render.rs:3189` に `ime_preedit_overlay` ── preeditのオーバーレイ描画専用のパスがrender.rs内に明確に分離されている。
  - `desktop_app/src/text_input.rs:672` には `EntityInputHandler` 実装をマクロ化する仕組み(`macro_rules!`)もあり、汎用テキスト入力コンポーネントとターミナルのIME実装で共通化を図っている。

**【2026-07-10 訂正】** 初版は「kitty keyboard protocolへの言及はtermy側にもなし(`rg -ni kitty`で0件)」と報告したが、これは**誤り**(オーナーの指摘を受けてクローンを再検証)。termyは `crates/core/src/keyboard.rs`(1,422行)に**モード対応のキーエンコーダを丸ごと実装している**: `TerminalKeyboardMode::from_term_mode(TermMode)` がDECCKM(`APP_CURSOR`)とkittyの全progressive enhancementフラグ(`DISAMBIGUATE_ESC_CODES`/`REPORT_EVENT_TYPES`/`REPORT_ALTERNATE_KEYS`/`REPORT_ALL_KEYS_AS_ESC`/`REPORT_ASSOCIATED_TEXT`)をミラーし、enhanced/basicの2経路でエンコードする。view側(`desktop_app/src/terminal_view/interaction/input.rs`)はペイン毎に`Terminal::keyboard_mode()`でライブなモードを引いて経路を選び、**純粋なモディファイア遷移のkittyイベント合成**(`modifier_transition_events`、GPUIがモディファイア単独の押下/解放を別イベントで届けるため)や**対になっていないkitty releaseのドロップ**というエッジケースまで処理している。Zed側(pinしたrev)にkittyの言及がないのは再検証でも確認できた。

Horizon的な含意: エンコード自体は`horizon-terminal-core`(`protocol/kitty_keyboard.rs`)が既に持つので、termyのエンコーダを借りる必要はない。ただし**「viewがいつテキスト入力パイプラインではなくKey経路に載せるべきかをどう知るか」**という統合問題(スパイクS2で記録した課題)に対して、termyは「viewがターミナルのライブなモードフラグを参照して分岐する」という答えを実地で示しており、`TerminalFrame`が`mouse_reporting`を運んでいるのと同型の「kittyフラグのフレーム搭載」がそのまま正解形であることを裏付ける。モディファイア遷移合成とrelease処理は、Horizonが本実装で踏む前にtermy(MIT)で確認できる。

---

## 3. 横断的に見えたパターン

1. **グリッド描画は例外なく「カスタム`Element`+`prepaint`/`paint`+`shape_line`によるバッチ化テキストラン」方式。** セル毎のdiv/子要素方式を採用したプロジェクトは1つもなかった(想定通り)。同一スタイルの隣接セルをRLE的に1つの`TextRun`にまとめてから`shape_line`を1回呼ぶ最適化は、Zed・zortax・termyの3者で収斂している。背景色も同様に矩形マージしてquad数を削減する。
2. **IME配線には2世代のAPIがある。** Zedは`InputHandler`を実装した独立の小さなアダプタ構造体を都度生成する古い様式。Xuanwo/gpui-ghostty・termyは`EntityInputHandler`をビューEntityに直接実装し、`window.handle_input(&focus, ElementInputHandler::new(bounds, view), cx)`で配線する新しい様式(後者の方が定型的で書きやすい)。**preeditはPTYに一切送らないクライアントサイドのみの状態**(`ime_marked_text: Option<String>`のような単純な文字列フィールド)として持ち、`paint()`内でカーソル位置に下線付きテキストとして上書き描画するのが共通パターン。確定(`replace_text_in_range`)は素朴に生バイトをPTYへ書く。
3. **kitty keyboard protocol は termy が完全実装している**(初版の「どのGPUI実装にも存在しない」は誤りで、2026-07-10に訂正 — §2.5の訂正注記を参照)。Zed・zortax・gpui-ghostty(GPUI層)には存在しない。Horizonはエンコードをhorizon-terminal-coreが担うため借りる必要はないが、view層のモード分岐・モディファイア遷移・release処理の設計はtermy(MIT)が最良の参照。
4. **再描画のパフォーマンス戦略は3段階ある。** (a) Zed/zortax: 毎回可視グリッド全体を再構築しつつイベントをcoalesce、(b) Xuanwo/gpui-ghostty: バックエンド(Ghostty)が提供する行単位dirty setを取得、(c) termy: **alacritty_terminal自体のdamage APIを使い列範囲まで絞り込む**。horizon-terminal-coreはtermyと同じ`alacritty_terminal 0.26`系なので、(c)の手法がそのまま横展開できる可能性が高い。
5. **「GPUIでどう描くか」という問いへの根本的に違う答えもある。** con-terminalのmacOSパスはGPUI Elementでの描画を放棄し、ネイティブNSView(Metal)埋め込みで済ませている。これはHorizonにとって「もしS1でGPUI Elementの手書きレンダリングが難航したら、ネイティブビュー埋め込みも保険として存在する」という参考情報にはなるが、macOS専用の抜け道でありクロスプラットフォーム方針とは相容れない。

---

## 4. Horizonスパイクへの提言

### S1 — グリッド描画Elementの設計

- Zed/zortax/termy収斂の**「行ごとにグループ化 → 同一スタイル連続セルをバッチ化した`TextRun` → `window.text_system().shape_line()`を1バッチ1回」**方式をそのまま採用する。ゼロから設計し直す必要はない。
- 背景色は別パスで隣接矩形マージ後にquad化(`fill()`)。
- 再描画最適化は**termyの列単位damage方式を第一候補**とする。horizon-terminal-coreは同じalacritty_terminal 0.26系なので`Term::damage()`(もし公開APIとして生えていれば)をそのまま使える可能性が高い。まずAPI存在確認をS1の最初のタスクにするとよい。ダメならZed方式(イベントcoalesce + フル可視領域再構築)にフォールバックする。カーソルブリンク等の頻繁な単独更新のために「全体差分」と「カーソルのみ」の2レーン持ちにするのは3プロジェクトとも実質同じ発想。

### S2 — 入力マッピング

- キーストローク→エスケープシーケンスの変換テーブルはZedの`mappings/keys.rs`(GPL、コピー不可だが「alacritty本家のソースを読んで作った」と明記された表構造なので、**alacritty本家(alacritty/alacritty)のキーマッピング表を直接参照して独自に書き起こす**のが安全な進め方)。
- horizon-terminal-coreがkitty protocolを既に持つため、GPUI層が新規実装すべきなのは「GPUIの`KeyDownEvent`/`Modifiers`からkitty encodingの入力へブリッジする薄い層」のみ。**この層の参照実装はtermyにある**(2026-07-10訂正 — §2.5参照): ライブなモードフラグによるテキスト経路/Key経路の分岐、モディファイア遷移イベントの合成、release対応まで実地のコードで確認できる。
- マウス報告(SGR/UTF8/Normalモード)はZed `mappings/mouse.rs`の設計思想(GPLだが定数はほぼalacritty本家由来の公開仕様)を踏襲すればよい。

### S3 — IME配線(go/no-go判定の核心)

**推奨する実装の型は Xuanwo/gpui-ghostty と termy が共通して採る「`EntityInputHandler`直接実装+`ElementInputHandler`アダプタ」方式。** 具体的には:

1. ターミナルビューのEntity(例: Horizonの`TerminalView`相当)に直接 `impl EntityInputHandler for TerminalView` を実装する。必要メソッドは `selected_text_range` / `marked_text_range` / `text_for_range` / `replace_text_in_range`(確定) / `replace_and_mark_text_in_range`(preedit更新) / `unmark_text` / `bounds_for_range`(IME候補ウィンドウ位置) / `character_index_for_point`。
2. preeditは`Option<String>`のフィールドとしてビュー側に持つだけでよい(PTYには一切送らない)。確定時(`replace_text_in_range`)はマークをクリアしてから生のUTF-8バイト列を直接PTYへ書く(`term.input(text.into_bytes())`相当)。
3. `paint()`内、通常のグリッド描画の**後**に、marked_textがあればカーソル位置に「背景色quadで下地を隠す→下線付きTextRunを`shape_line`して描く」を行い、marked_text表示中は通常カーソルを描かない。
4. `paint()`の対話ブロック内で `window.handle_input(&focus_handle, ElementInputHandler::new(bounds, view_entity.clone()), cx)` を1行呼ぶだけで配線完了。
5. **見落とし注意点(Xuanwo/gpui-ghosttyのテストが明示)**: GPUIの`Keystroke`は`is_ime_in_progress()`を持つ。IME変換中のキーイベントは通常の`on_key_down`処理をスキップしないと、変換候補選択中のキー入力がターミナルにそのまま流れてしまう可能性がある。Horizonの既存Floem実装で踏んだであろう罠と同種のものがGPUI側にも存在することは、あらかじめS3のテスト項目に入れておくべき。
6. `bounds_for_range`(IME候補ウィンドウ位置)は3プロジェクトとも「カーソルのセル位置 + `range_utf16.start * cell_width`」という単純計算で足りている。日本語入力でIMEツールチップが文字位置からズレる問題(termyのROADMAP.mdにも "Fix IME candidate window position (CJK preedit misalignment)" という完了項目がある=実運用で踏んだ既知の罠)に注意。

**最良の合法的参照コードベース**: **`lassejlv/termy`**(MIT、`alacritty_terminal 0.26`でバージョン一致、`EntityInputHandler`+`ElementInputHandler`のIME配線、alacritty自体のdamage APIを使った行/列単位再描画)。バックエンド・GPUIバージョン・IME様式のいずれの軸でも現時点でHorizonに最も近い実装であり、設計の型として最も直接的に踏襲できる。次点は**`Xuanwo/gpui-ghostty`**(Apache-2.0、IME配線がtermyよりコンパクトで追いやすく、`is_ime_in_progress()`ガードの必要性を明示するテストコードがある点で「読んで理解する」教材として優秀)。Zedの2クレートはGPLのため実装の型を学ぶ参照専用に留め、コードは一切引用・移植しないこと。`nowledge-co/con-terminal`の`terminal_ime.rs`(MIT、171行のみ)はバックエンド非依存の再利用可能な抽象として、ライセンス上コピーしても問題ない候補だが、Ghostty系プロジェクトの一部として書かれているため単独での可読性はやや落ちる。

### 総括

S1(グリッド描画)・S2(入力)は「先行事例に強い収斂パターンがあり、実装の型がほぼ決まっている」領域であり、技術的な不確実性は低い。S3(IME)についても、`EntityInputHandler`+`ElementInputHandler`という定型パターンと、preeditはクライアントサイドのみで持つという設計、IME変換中のキーイベント除外というGPUI特有の罠まで含めて、複数の独立実装から同一の解法が確認できた。**「GPUIでフレームワークと戦う」リスクは、少なくとも設計パターンの存在という意味では低いと判断できる。** 残るのはCJKプリエディトの位置合わせなど細部の実地検証であり、これはHorizon側でS3を実際に組んで既存Floem実装のIMEテストケースと突き合わせる以外に確認しようがない。

---

## 付記(調査方法・制約)

- Zedは`crates/terminal`・`crates/terminal_view`・両ライセンスファイルのみをsparse-checkoutでフェッチ(フルクローンは回避)。
- `zortax/gpui-terminal`・`Xuanwo/gpui-ghostty`・`nowledge-co/con-terminal`・`lassejlv/termy`は`--depth 1`でスクラッチパッドにクローンし`rg`で直読。
- `lassejlv/termy`の`crates/desktop_app/src/terminal_view`は44,693行あるが、その大半(タブ・コマンドパレット・ワークスペース管理等)はHorizonのスパイクの対象外機能であり、上表の「核となるビュー層」概算はグリッド/入力/IMEに関わるファイルのみを合算した数値。
- 各リポジトリの最終更新: `zortax/gpui-terminal` 2026-01、`Xuanwo/gpui-ghostty` 2026-04、`nowledge-co/con-terminal` 2026-06、`lassejlv/termy` 2026-07-08(本調査時点で直近)── いずれも活発にメンテされている。
- 時間制約により`lassejlv/termy`の深掘りは終盤に圧縮して実施したが、IME配線・damage APIについては該当ファイルを直接読んで確認済み。他プロジェクトほど周辺機能(tmux連携等)までは追えていない。
