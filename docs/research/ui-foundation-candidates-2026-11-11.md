# UI の土台の候補調査(2026-11-11)

Horizon の画面全体の土台を gpui から置き換えるとしたら何がありうるか、の調査記録。
置き換えるかどうかの判断はここでは行わない。

## 調査の観点

きっかけは、gpui の状態更新(`notify` でビュー単位に描き直す)への不満と、記事
[A Critique of GPUI](https://blog.zortax.de/posts/a-critique-of-gpui)(2026-11-08)。
求めているものは次の 4 つ。

1. 状態: 一度だけ走るコンポーネント+signal(読んだ場所だけが更新される)
2. スタイルとレイアウト: CSS 的に書けること
3. 増分更新: 状態だけでなく、レイアウトと描画も変わった部分だけで済むこと
4. gpui-component が埋めていたもの(テキスト入力と IME、ボタンやモーダルなどの部品)を誰が持つか

記事の要点は「signal にしただけでは速くならない。レイアウトと描画が全体を走査するなら、そこで時間を使う」。
したがって 3 を、状態・レイアウト・描画の 3 段に分けて見る。

## 確かめ方と確かさ

- **機械的に取ったもの**: GitHub API による活動状況(2026-11-11 取得)、crates.io の公開日。
- **一次情報を読んだもの**: 各リポジトリの README、設計文書、`Cargo.toml`。
- **こちらで動かして確かめたものは無い。** 性能と機能についての記述は、各プロジェクト自身の説明か、上の記事の計測による。
  記事の計測は著者(zgui の作者)の未公開ハーネスによるもので、各フレームワークの作者のレビューを受けていない(記事自身の記載)。

## 活動状況(2026-11-11、直近 90 日)

| リポジトリ | コミット | 最新リリース | スター | 作成 |
|---|---:|---|---:|---|
| zortax/zgui | 769 | v0.1.0(2026-10-15) | 302 | 2026-08-02 |
| zortax/cephal(zgui のレイアウト) | 164 | 0.2.0(2026-10-22) | 4 | 2026-09-10 |
| lapce/floem | 1 | v0.2.0(2024-11-15) | 4322 | 2023-01-31 |
| DioxusLabs/blitz | 453 | タグ v0.3.0-beta.4 | 4442 | 2022-02-28 |
| marc2332/freya | 336 | v0.5.1(2026-11-08) | 3382 | 2022-07-27 |
| linebender/xilem(Masonry を含む) | 18 | v0.4.0(2025-10-29) | 5603 | 2022-11-16 |
| vizia/vizia | 10 | 0.4.0(2026-04-23) | 2355 | 2021-09-17 |
| slint-ui/slint | 2272 | v1.18.1(2026-09-21) | 24559 | 2020-05-04 |
| iced-rs/iced | 333 | 0.14.0(2025-12-07) | 32000 | 2019-07-15 |
| longbridge/gpui-fast | 336 | v0.1.14(2026-11-09) | 577 | 2026-09-24 |
| linebender/vello | 167 | v0.11.0(2026-10-02) | 4484 | 2020-04-15 |
| linebender/parley | 160 | v0.12.0(2026-10-09) | 785 | 2021-06-04 |
| DioxusLabs/taffy | 162 | v0.15.1(2026-10-25) | 3770 | 2022-01-04 |
| rust-windowing/winit | 70 | v0.31.0-beta.4(2026-10-24) | 6265 | 2016-02-23 |
| AccessKit/accesskit | 56 | 2026-10-25 | 1558 | 2021-06-22 |
| leptos-rs/leptos(`reactive_graph` を含む) | 185 | v0.9.0-rc1(2026-10-31) | 21500 | 2022-07-31 |

## 観点ごとの一覧

「状態」は、値が 1 つ変わったときに何が走るか。記事の分類による。

| 候補 | 状態 | スタイル・レイアウト | レイアウトの増分 | 描画の増分 | 部品 |
|---|---|---|---|---|---|
| zgui | signal、一度だけ走る | CSS(Stylo) | あり(自前の cephal)と説明 | あり(描画記録の保持+損傷矩形)と説明 | shadcn 相当の一式(`zgui-ui`) |
| Floem | signal、一度だけ走る | Rust のスタイル API、Taffy | 記事では全体計算(3,000 行でラベル 1 つに約 14ms) | 未確認 | 基本的な部品 |
| Blitz + Dioxus Native | signal、コンポーネントを再実行(仮想 DOM) | HTML/CSS(Stylo+Taffy) | 未確認 | 未確認 | dioxus-components |
| Freya | signal、コンポーネントを再実行 | Rust の属性、自前のレイアウト | 記事では全体を訪問(57,050 ノードを見て 14 を再計算) | 未確認 | 一式あり |
| Xilem / Masonry | ビューの木を作り直して差分 | Rust の API | Masonry は保持したウィジェット木 | 未確認 | 少ない |
| Vizia | signal(2026-03 に移行)+モデルとイベント | CSS 風 | 記事では全体を走査(120,091 エンティティ) | 未確認 | 一式あり |
| Slint | 独自言語のプロパティ束縛 | 独自言語 | 記事ではラベル 1 つに 172ms(3,000 行) | 未確認 | 一式あり |
| Iced | Elm 型(全体の `view` を再実行) | Rust の API | — | — | 一式あり |
| gpui-fast | gpui のまま(ビュー単位) | gpui のまま | ビュー・レイアウトノード・文字計測を保持 | スクロール層のタイルを再利用 | gpui-kit がそのまま使える |

## 候補ごとの記録

### zgui(zortax/zgui)

求めている 4 点すべてに、説明の上では当たる。上の記事の著者のフレームワーク。

- **構成**(README): 「components + reactive_graph signals → retained document → Stylo cascade → cephal layout + Parley text → retained paint recordings → wgpu renderer + Vello → persistent render target」。
  各段が「次の段に、やるべき仕事だけを渡す」と説明している。
- **状態**: Leptos の `reactive_graph`(`=0.2.14` で固定)。仮想 DOM なし。コンポーネントは一度だけ走る。
- **CSS**: Stylo(`=0.19.0` で固定)。セレクタ、カスケード、カスタムプロパティ、メディアクエリ、flexbox、grid、遷移、アニメーション。
  `docs/parity.md`(生成物)によると、エンジンが持つ 250 のロングハンドのうち 179 を実装、71 は解析とカスケードのみ。
  `:has()` などは Stylo の servo ビルドの制約で使えず、フォークはしない方針と書いている。
- **レイアウト**: `cephal` 0.2.0(同じ作者、crates.io に 2026-10-10 公開)。「Retained, incremental CSS layout (block, flexbox, grid) with spineless invalidation」。
  過去の検索結果には「Taffy for layout」とあり、途中で置き換えた形跡がある(経緯は確かめていない)。
- **描画**: wgpu 29、Vello 0.9。フレームをまたぐ描画先を持ち、損傷矩形の外のピクセルは前の値を保つ、と説明。
- **部品**: `zgui-ui` に input、textarea、select、combobox、dialog、popover、menu、tabs、sidebar、data_table、toast、tooltip、resizable、scroll_area、virtualize など約 60。
- **入力**: IME の前編集(preedit)を扱うコードが `zgui-input` と `zgui-edit` にある。
- **プラットフォーム**(README): Windows、macOS、Linux(Wayland と X11)、headless。アクセシビリティ(AccessKit)、クリップボード、複数ウィンドウ、独自のウィンドウ装飾。
- **独自の描画**: canvas、SVG、カスタム要素、アプリケーションのシェーダ、wgpu の面の埋め込み。
- **テスト**: headless のプラットフォームと testkit のクレートがある。性能は `cargo xtask perf` が CI の一部で、帯を外れると失敗する(`docs/performance.md`)。
- **この上で作られたもの**(README): コードエディタ(zdt: 分割、ファイル木、LSP、Git、ターミナル、コーディングエージェントのワークフロー)、
  埋め込み用のターミナルエミュレータ(zgui-terminal)、埋め込み用のコードエディタ(zgui-editor)、Spotify クライアントなど。
  いずれも同じ作者か近い人のもので、zdt と zgui-terminal と zgui-editor は 2026-09 以降の push がない。
- **見当たらなかったもの**: Markdown を表示する部品(リポジトリ内の検索で該当なし)。`wasm32` 向けの対応。

リスクとして読み取れること。

- 作成から 3 か月(2026-08-02)。v0.1.0 が 2026-10-15。README に「experimental and changes often. Expect breaking changes between 0.x releases」。
- コミットの大半が 2 人(636 と 181)。
- 開発は nightly(`nightly-2026-04-16`)。公開クレートは stable(`rust-version = "1.90"`)でビルドできると書いている。
- Stylo と `reactive_graph` を完全一致で固定している。fontique は `vendor/` に持っている。
- リポジトリに `CLAUDE.md` がある。

### Floem(lapce/floem)

以前使っていた土台。離れた理由の一つだった上流の更新は、今も遅い。

- 直近 90 日のコミットは 1 件(2026-09-24)。その前は 2026-06-21、2026-05-11。最新リリースは 2024-11-15 の v0.2.0。
- README: ビューの木は一度だけ作る。レイアウトは Taffy。描画は vger、vello、Skia(AnyRender)、tiny-skia から選ぶ。
- 記事の計測: ラベル 1 つの変更で、反応の処理は 0.01ms、その後のレイアウトに約 14ms(3,000 行)。
  「Taffy does not know that one leaf moved」。同じ変更が再描画されない既知の不具合がある、とも書いている。

### Blitz + Dioxus Native(DioxusLabs/blitz)

HTML/CSS の描画エンジン。Stylo+Taffy+Parley+Vello という部品の組み合わせは zgui と近い。

- README: 「Blitz is currently in a **beta** state ... usable for making apps if you are an early adopter」。
- 対話できるのは `dioxus-native`(Dioxus の仮想 DOM を描く)経由。Dioxus は signal を持つが、コンポーネントを再実行する型。
- `blitz-dom` を土台にして、状態の層だけ別のもの(一度だけ走る signal)を載せる、という使い方は README からは読み取れない(確かめていない)。
- Markdown と HTML を表示するフロントエンド(`blitz`)があるが、対話はできないと書いている。

### Freya(marc2332/freya)

- 状態は signal だが、コンポーネントを再実行する(`use_state`)。記事は「細かい更新には小さいコンポーネントが要る」点を gpui と同じ問題として挙げている。
- 記事の計測: 3,000 行でキー入力 1 回が 1.2ms。ラベル 1 つで、5 つの変更を出した後に 57,050 のレイアウトノードを訪問。
- 描画は Skia。コードエディタ用の部品(Rope+tree-sitter)がある。

### Xilem / Masonry(linebender/xilem)

- README: 「An experimental Rust architecture for reactive UI」。直近 90 日のコミットは 18 件。
- Xilem はビューの木を作り直して差分を取る型で、signal ではない。Masonry は保持したウィジェット木を持つ下の層。

### Vizia、Slint、Iced

- Vizia: 2026-03 に lens から signal へ移行(記事)。直近 90 日のコミットは 10 件。
- Slint: 活発(2272 件)。独自の言語で書く。ライセンスは GPL・ロイヤリティフリー・商用の選択式(SPDX は NOASSERTION)。
- Iced: Elm 型で、求めている状態モデルと異なる。

### gpui-fast(longbridge/gpui-fast)

gpui を替えずに、同じ問題を軽くする選択肢。

- README: 「GPUI extracted from the Zed monorepo as a standalone workspace, with the layout engine reworked to keep what did not change between frames」。
  変わらないビュー、レイアウトノード、文字計測を再利用する。gpui-kit から使うための互換設定がある。
- 作成は 2026-09-24。直近 90 日のコミットは 336 件。
- 記事の計測: 3,000 行でラベル 1 つが 8.4ms から 3.4ms。状態を各パネルが持つ場合は 83% 減、共有モデルの場合は 11% 増。
- 状態モデルは gpui のまま(最小の単位はビュー)。書き方の問題は残る。

## 部品を組んで自前で持つ場合

zgui の構成が、そのまま「何を組むか」の一例になっている。

| 役割 | 使える部品 | 増分か |
|---|---|---|
| ウィンドウと入力 | winit(0.31 はベータ) | — |
| signal | `reactive_graph`(Leptos)、sycamore-reactive、floem_reactive | 状態は増分 |
| CSS | Stylo | セレクタの依存で再スタイルを絞れる(zgui の説明) |
| レイアウト | Taffy、cephal | Taffy はキャッシュを持つが、記事では Floem と Freya が全体を訪問している。cephal は増分をうたう(公開から 1 か月) |
| 文字 | Parley(レイアウト、選択、編集)、cosmic-text | 段落単位の再整形は上の層の仕事 |
| 描画 | Vello(GPU)、wgpu | 損傷矩形と描画先の保持は上の層の仕事(zgui は自前で持っている) |
| アクセシビリティ | AccessKit | — |

zgui のクレート数は 60 を超える(DOM、スタイル、レイアウト、描画記録、レンダラ、プラットフォーム、入力、編集、部品、テスト基盤)。
同じものを自前で揃える場合の規模の目安になる。

## 調べていないこと

- zgui を実際にビルドして動かすこと、Horizon のビューを 1 つ移してみること。
- zgui の日本語入力(IME)の実際の挙動、フォントのフォールバック。
- zgui の上でのターミナル描画の性能(zgui-terminal の実装は読んでいない)。
- 各候補の macOS での状態。
- 記事の計測の再現。

## 追補: 小さなクレートを組み合わせる場合(同日)

zgui は自前で持つ範囲が広い(Wayland・DRM・evdev・libinput・xkb のプラットフォーム層、レイアウト、描画記録、
レンダラ、入力、編集、部品、テスト基盤)。小さな責務のクレートを組み合わせて同じことができるかを調べた。

### Taffy は増分か(こちらで計測)

Taffy 0.15.1 で、行(行 = 伸びるラベル+固定のラベル)を縦に並べた木を作り、ラベル 1 つの文字幅を変えて
`mark_dirty` → 再レイアウトした。release、中央値(51 回)、マイクロ秒。括弧内は文字の計測関数が呼ばれた回数。

| 行(ノード) | 丸めあり・親 1 つ | 丸めなし・親 1 つ | 丸めなし・50 行ごとにグループ | 丸めあり・50 行ごとにグループ |
|---|---:|---:|---:|---:|
| 100(301) | 35.8(9) | 8.4(9) | 10.6(9) | 19.1(9) |
| 1,000(3,001) | 379.9(9) | 72.0(9) | 18.7(9) | 125.6(9) |
| 3,000(9,001) | 561.7(9) | 238.1(9) | 22.7(9) | 373.5(9) |
| 10,000(30,001) | 3607.2(9) | 908.2(9) | 23.3(9) | 1665.7(9) |

何も変えずに再レイアウトした場合は、丸めなしで 0.04(ルートのキャッシュが当たる)、丸めありで 3,000 行のとき約 340。

読み取れること。

- 文字の計測はキャッシュされる。葉 1 つの変更で計測は 9 回で、木の大きさによらない。
- 既定で有効な丸め(`round_layout`)は、全ノードを再帰的に訪問する。`disable_rounding` で外せる。
- 丸めを外すと、時間は「変更箇所の祖先が持つ子の数」に比例する。親 1 つに 3,000 行なら約 240、50 行ごとに分ければ約 20 で、行数によらない。
- 記事が Floem について書いた「ラベル 1 つでレイアウト約 14ms(3,000 行)」は、Taffy 単体のこの値(0.2〜0.6ms)よりずっと大きい。
  差が Floem 側のどこから来るかは調べていない。

計測コードは作業用ディレクトリにあり、リポジトリには入れていない。

### 責務ごとの部品

| 責務 | 部品 | 状態 |
|---|---|---|
| レイアウト | Taffy | 上の計測のとおり、丸めを自分で持てば増分になる |
| レイアウト | cephal 0.2.0 | zgui から独立した単独のクレート(依存は任意の serde と taffy のみ)。MIT/Apache。README: 汚れたノードを深い順に処理し、答えが変わらなければ伝播を止める(Kirisame ら「Spineless Traversal for Layout Invalidation」PLDI 2025 に従う)。Taffy 0.15 の Chrome 由来フィクスチャ 6,504 件を通す、と書いている。公開から 1 か月 |
| CSS+文書の木+レイアウト | `blitz-dom` | Stylo+Taffy+Parley を束ねた層。Dioxus には依存しない。`DocumentMutator` で文書を書き換える API がある。Stylo の `RestyleDamage` を拡張した損傷ビットで、箱の再構築とレイアウトを絞る増分モード(`incremental_layout`)がある |
| 描画の抽象 | anyrender | Vello(GPU・CPU・hybrid)と Skia のバックエンド。損傷矩形の概念は見当たらない |
| 描画 | Vello | 現在の利用者向けは「Sparse Strips」方式の `vello_cpu` と `vello_gpu`。以前の計算シェーダ方式は `research/` に移っている |
| 文書から描画命令へ | `blitz-paint` | `paint_scene(scene, doc, ...)` で文書全体を描く形。部分的な再描画の仕組みは見当たらない |
| 当たり判定、フォーカス、ホバー、ドラッグ、オーバーレイ、仮想リスト、選択 | understory | ワークスペースの版は 0.1.0。`understory_virtual_list` は crates.io に 0.1.2(2026-05-22)。`understory_box_tree` は「commit で粗い損傷領域を返す」と説明 |
| signal | `reactive_graph` | zgui もこれを使っている |
| 文字 | Parley | 選択と編集のユーティリティを含む。Blitz に Parley 製のコードエディタ(`parley_code_editor`)がある |
| ウィンドウ、アクセシビリティ | winit、AccessKit | |

### 部品が見当たらないもの

- **損傷矩形と、フレームをまたぐ描画先の保持**: anyrender にも `blitz-paint` にも見当たらない。zgui は自前で持っている。
- **signal と文書の木をつなぐ層**: `blitz-dom` の `DocumentMutator` に signal から書き込む層は、既存のものが見当たらない(Dioxus は仮想 DOM の差分から書き込む)。
- **部品一式**(入力欄、ダイアログ、メニューなど): 土台に依存しない形のものは見当たらない。understory がその下の状態機械(オーバーレイ、フォーカス)を持つ。

### 調べていないこと(追補分)

- `blitz-dom` の増分モードが既定で有効か、どこまで安定しているか。Taffy の丸めを全体で走らせているか。
- Vello の Sparse Strips 方式に、部分的な再描画の仕組みがあるか(コード検索の回数制限で確認できなかった)。
- understory の各クレートの公開状況と API の安定度。
- cephal を zgui の外で使った例があるか。
