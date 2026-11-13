# デザインシステムの動向(2026-11-13)

Horizon の全 view を「view ごとの style sheet」に書き直す前に、デザインシステムを決めておくための調査記録。
結論や方針は書いていない。最後の節に、Horizon に当てはめたときに決めることになる論点だけを並べる。

## 確かめ方と確かさ

- 外部の動向は、3 つの観点(スタイルの書き方、トークンとテーマ、部品の層)を別々の調査に分けて調べた。
  各項目に、**(a)** 一次情報(公式の文書、発表、仕様、リポジトリ)を読んで確認したもの、**(b)** 二次情報か検索結果の要約にとどまるもの、の印を付ける。
- Horizon の現状と、Blitz が解釈できる CSS は、手元のコードで数え、実際に動かして確かめた(検証ブランチ `scoped-styles`)。
- 保守性について、手法どうしを測って比べた資料は見つからなかった。経験談と各プロジェクトの自己申告が中心である。

## 1. スタイルの書き方

### utility-first

- **Tailwind CSS v4**(2025-01-22)**(a)**: 設定を CSS の中に書く(`@theme`)。トークンは `:root` の CSS 変数として出る(`--color-*` など)。
  土台は `@layer theme, base, components, utilities`、`@property`、`color-mix()`。余白は 1 つの変数 `--spacing` の倍数。
  https://tailwindcss.com/blog/tailwindcss-v4
- **class の見つけ方** **(a)**: ソースを文字として走査する。class 名は完全な文字列で書かれている必要があり、組み立てた名前は生成されない。ビルド時にソースを読む手順が要る。
  https://tailwindcss.com/docs/detecting-classes-in-source-files
- **部品ごとの sheet との併用** **(a)**: 公式の文書は勧めていない(sheet ごとに別の Tailwind の実行になり、`@theme` が共有されない)。その場合は CSS 変数(`var(--color-…)`)を直接使うよう案内している。
  https://tailwindcss.com/docs/compatibility
- **Node 無しの生成器**: encre-css は Tailwind v4 互換で、0.21.1(2026-09-09)まで更新がある。ライブラリで、マークアップの文字列を渡して生成する。利用は少数 **(a/b)**。railwind は 0.1.5(2023-02)で止まっている **(b)**。
  https://lib.rs/crates/encre-css
- **UnoCSS** **(a)**: 規則をすべて preset から与える、必要な分だけ生成する原子的なエンジン。https://unocss.dev/guide/

### 型つき・ビルド時生成

- 実行時にスタイルを生成する手法は退いた。styled-components は 2025-03 に保守モード **(b)**。
- 残ったのはビルド時に静的な CSS を出すもの: StyleX(Meta。宣言 1 つを 1 class にする。「CSS のサイズが 80% 減った」と報告。基準は示されていない)**(a)**、Panda CSS v2(2026-09-29。`@layer` と `@property` を使う。Node が要る)**(a)**、vanilla-extract、CSS Modules。
  https://engineering.fb.com/2025/11/11/web/stylex-a-styling-library-for-css-at-scale/ / https://panda-css.com/blog/panda-css-v2
- GitHub の Primer は styled-components から CSS Modules へ移った **(b)**。
- JavaScript に依らない考え方として残るもの: トークンを型つきの定数として持ち CSS 変数に落とす、variant、layer による優先順の固定、原子的な出力(利点は配信サイズ)。

### variant

- cva、tailwind-variants、vanilla-extract の `recipe()`、Panda の recipe **(a)**: 共通の形は `base` / `variants` / `compoundVariants` / `defaultVariants`。
  variant は「入力の値 → 完全な class 名」の表で、マークアップとは別に宣言する。tailwind-variants は部品の中の名前つきの部分(slot)も扱う。
  https://cva.style/docs / https://vanilla-extract.style/documentation/packages/recipes/

### 標準の CSS で解けるようになったこと

ブラウザでの状況(web-features explorer、2026-11-13 閲覧)**(a)**:

| 機能 | 状況 |
| --- | --- |
| `@layer` | 広く利用可能(2024-09 から) |
| container query | 広く利用可能(2025-08 から) |
| `color-mix()` | 広く利用可能(2025-11 から) |
| 入れ子 | 広く利用可能(2026-06 から) |
| `:has()` | 広く利用可能(2026-06 から) |
| `light-dark()`、`@property`、相対色の構文 | 新しく利用可能(2024 年) |
| `@scope` | 新しく利用可能(2026-03。Chrome 143、Firefox 146、Safari 26.4) |
| anchor positioning | 資料が食い違う(未解決) |

### 批判と引き換え

- State of CSS 2025 **(b)**: Tailwind は最も使われている framework。同時に「Tailwind の使いすぎ」が不満として挙がっている。
- 個人の移行記(Julia Evans、2026-05-15)**(a)**: ビルド手順が要ること、普通の CSS と混ざること、変わった CSS が書きにくいことを理由に離れ、「部品 1 つに class 1 つ、ファイル 1 つ、入れ子、色のトークンのファイル」にした。引き換えとして、隔離が規律頼みになることを挙げている。
  https://jvns.ca/blog/2026/05/15/moving-away-from-tailwind--and-learning-to-structure-my-css-

## 2. トークンとテーマ

### DTCG(W3C Design Tokens Community Group の書式)

- 2025.10(2025-10-28)が最初の安定版 **(a)**。Style Dictionary、Tokens Studio、Terrazzo が参照実装。
  https://www.w3.org/community/design-tokens/2025/10/28/design-tokens-specification-reaches-first-stable-version/
- 型: color、dimension(値と単位。単位は px か rem)、fontFamily、fontWeight、duration、cubicBezier、number。合成型: border、shadow、typography、transition など。別名は `{group.token}` **(a)**。
- 色は 14 の色空間(oklch、oklab、display-p3 を含む)**(a)**。
- 明暗などの切り替えは Resolver の仕様で表す(token の集合、切り替えの軸と各値、適用の順序)**(a)**。
- 道具: Style Dictionary 5.x は 2025.10 に段階的に対応中。Terrazzo 2.0 は Resolver まで対応と記載 **(a)**。Figma の書き出しは部分的との報告 **(b)**。

### トークンの階層と名前

- **GitHub Primer** **(a)**: base(`base-color-green-5`)/ functional(`bgColor-inset`、`borderColor-default`)/ component(`button-primary-bgColor-hover`)。色の variant は `default` / `muted` / `emphasis`。
  https://primer.style/product/primitives/token-names/
- **IBM Carbon** **(a)**: 役割で名付ける(`$background`、`$layer-01..03`、`$border-subtle-01`、`$text-primary`、`$focus`)。状態は接尾辞(`-hover`、`-active`、`-selected`、`-disabled`)。
  https://carbondesignsystem.com/elements/color/tokens/
- **Atlassian** **(a)**: `--ds-background-neutral`、`--ds-space-200`。テーマは `<html>` の属性で切り替える。
- **Radix Themes** **(a)**: `--accent-1..12`、`--gray-1..12`、透過の段、`--color-surface` など。focus の線は `--focus-8`。
- **Tailwind v4** **(a)**: `@theme` の名前空間(`--color-*`、`--text-*`、`--spacing-*`、`--radius-*`、`--shadow-*`)。
- Material 3、Polaris、Spectrum 2 の変数名は確認できなかった。

### 色

- **Radix の 12 段** **(a)**: 1–2 背景、3–5 UI 要素の地(通常 / hover / 押下・選択)、6–8 境界線(弱い / 操作できる / 強い+focus)、9–10 塗りとその hover、11–12 文字(低コントラスト / 高コントラスト)。
  生成器は accent、gray、background、明暗から 12 段を出す。
  https://www.radix-ui.com/colors/docs/palette-composition/understanding-the-scale
- **Material の色の道具** **(a)**: 種の色 1 つ、変種、コントラストの段(-1〜1)から配色を導く。
- **Linear** **(a)**: テーマを 3 つの入力(base、accent、contrast)から LCH で生成。テーマごとに 98 個の変数を手で決めていたのを置き換えた(2024-03)。
  https://linear.app/now/how-we-redesigned-the-linear-ui
- **Tailwind v4** **(a)**: 既定の配色を OKLCH に移した。
- **コントラストの基準** **(b)**: APCA は WCAG 3 の草案から外れ、2026-04 時点で「未定」。実務の基準は WCAG 2 の比のまま。
- **明暗** **(a)**: 同じ変数を選択子の下で付け替えるか、`light-dark()`(`color-scheme` が要る。色にだけ使える)。

### 文字と余白

- **Carbon** **(a)**: 密度の高い UI 向けに 14px 基準の固定の段(`body-compact-01` は 14px / 行 18px)。式ではなく段の一覧。
- **Primer** **(a)**: 文字は rem、行の高さは単位なしで 4px の格子に合わせる。
- **Tailwind v4** **(a)**: 余白は 1 つの基準値の倍数。
- **Radix Themes** **(a)**: 全体の拡大率を 1 つの値で持つ(`scaling`)。

### 開発者向けの道具

- **Zed のテーマの schema(v0.2.0)** **(a)**: 面は 3 段(`background`、`surface.background`、`elevated_surface.background`)。要素の状態は `element.{background,hover,active,selected,disabled}` と、地を持たない要素用の `ghost_element.*`。境界線は `border`、`border.variant`、`border.focused`、`border.selected`。文字は `text`、`text.muted`、`text.placeholder`、`text.disabled`、`text.accent`。状態の色(error など)は地と境界線を持つ。ターミナルの ANSI 色、構文の色は別の枠。
  https://zed.dev/schema/themes/v0.2.0.json
- **VS Code** **(a)**: `領域.性質[状態]` の形(`list.activeSelectionBackground`)。ターミナルの 16 色は別の鍵。
- **JetBrains、Ghostty、Warp** **(a)**: いずれも UI の色、ターミナルの色、構文の色を分けている。

## 3. 部品の層

- **見た目を持たない部品** **(a/b)**: Radix Primitives、Base UI(2025-12-11 に 1.0)、React Aria Components、Ark UI / Zag.js(状態機械)、Headless UI。
  再利用の単位は「振る舞いとアクセシビリティ」で、見た目はアプリの側。状態は属性で表す(`[data-selected]`、`[data-pressed]`)。
  shadcn/ui は 2026-07 に既定を Radix から Base UI に替えた **(a)**。
- **ソースを自分のリポジトリに置く形** **(a)**: shadcn/ui の registry(部品、テーマ、依存、`cssVars` の `theme` / `light` / `dark`)。
  弱点として、更新の経路が無いこと、写しがばらけることが挙げられている **(b)**。
  Dioxus にも同じ形がある(`dx components add`。見た目なしの部品の上に載る)**(a)**。
- **ブラウザの新しい部品の仕組み** **(a)**: popover(2025-01 から利用可能)、invoker commands(2025-12 から)、見た目を変えられる select(一部のブラウザのみ)、anchor positioning(食い違いあり)。
  Blitz の公開している対応表は、anchor positioning、container query、`appearance` を未対応としている。https://blitz.is/status/css
- **Web Components のデザインシステム** **(a/b)**: Web Awesome(Shoelace の後継)は、トークン(`--wa-*`)、部品ごとの変数、`::part()`、`:state()` の 4 つを見た目の口にしている。Material Web は 2024-06 から保守モード。
- **Rust の UI** **(a)**: Dioxus 0.7 は Radix を基にした見た目なしの部品 28 個と、その上の shadcn 風の CSS。gpui-component は `gpui-base`(振る舞い)と `gpui-component`(見た目)の 2 層。Slint は `Palette` と `StyleMetrics`、egui は `Visuals` を部品が読む。
- **振る舞いの仕様** **(a)**: W3C の WAI-ARIA Authoring Practices Guide(APG)が 30 のパターンを定めている。複合の部品は Tab で止まる場所を 1 つにし、中は矢印で動く(roving tabindex か `aria-activedescendant`)。listbox は typeahead と Home/End を推奨。Radix は部品ごとに、従うパターンとキー操作の表を載せている。
  https://www.w3.org/WAI/ARIA/apg/patterns/

## 4. Horizon の現状

### トークン

`crates/horizon-theme`(色の計算。少数の種の色から OKLab で導く)と `crates/horizon-theme-css`(CSS 変数にする)が持っているもの:

- 色: 意味で名付けた約 40 個(`--background`、`--text`、`--text-muted`、`--text-subtle`、`--accent`、`--danger` ほか、`--border`、`--surface-panel` / `-raised` / `-chrome` / `-selected`、`--selection`、`--band`、`--chip-*-surface` / `-text`、`--tab-*`、`--on-accent`、差分の色)。
- 文字: 5 段(`--t1`、`--t2`、`--body`、`--meta`、`--micro` の大きさと行の高さ)。基準 14 の表を、使用中の文字サイズに比例させて整数に丸める。
- 長さ: 文字サイズに連動する 7 段(`--u4`、`--u8`、`--u12`、`--u16`、`--u24`、`--u28`、`--u32`)。
- フォント。
- 無いもの: 角丸、線の太さ、影、重なりの順序、動きの時間、要素の状態(hover / active / selected / disabled / focus)の体系。

### view の CSS での使われ方

12 ファイル、613 行、class 名 192 種。

| | トークンを使った箇所 | 直書きの箇所 |
| --- | --- | --- |
| 色 | 224 | 15(ほぼカラーピッカー) |
| 文字の大きさと行の高さ | 133 | — |
| 長さ | 159 | 218 |

直書きの長さの内訳(多い順): 2px ×41、1px ×38、4px ×29、8px ×24、6px ×17、12px ×15、16px ×10、20px ×8、3px ×6。
4 / 8 / 12 / 16 はトークンに同じ段があるのに直書きされている。`src/workspace/mod.rs`(shell)は長さのトークンを使っていない。

### Blitz が解釈できる CSS

固定している版(blitz 0.3.0-beta.4 から 58 コミット先、stylo cd230184)で、view の sheet と同じ入れ方をして実際に動かした結果。

| 状態 | 機能 |
| --- | --- |
| そのまま使える | `@layer`、入れ子、`:is()` / `:where()`、CSS 変数、`calc()` / `clamp()`、`@property`、grid、`oklch()`、`color-mix()`、`light-dark()`、相対色の構文、media query(配色、範囲の書き方)、論理プロパティ、`:focus-visible` |
| Stylo の設定を有効にすれば使える | `@scope`、`:has()`、`:nth-child(… of …)` |
| 使えない | container query(設定を有効にしても動かない)、anchor positioning |

`@layer` は、文書の中の `<style>` に書けば動く。起動時に文書全体へ渡す sheet(今のテーマと全 view の CSS の入れ方)の中で使うと、Stylo の内部の検査に引っかかって落ちる。

## 5. Horizon に当てはめたときの論点

決めてはいない。決めることになる点の一覧。

1. **トークンの語彙**: 今の約 40 個を保つか、参照できる体系(Zed の schema の面 3 段と要素の状態、Radix の 12 段、Primer の `default` / `muted` / `emphasis`)に寄せて組み直すか。要素の状態の体系が今は無い。
2. **長さの扱い**: 直書きの 218 箇所をどうするか。角丸、線の太さ、影、重なりの順序にトークンを設けるか。余白を段の一覧で持つか、1 つの基準値の倍数で持つか。
3. **派生色をどこで計算するか**: 今は Rust 側で全部計算して 16 進で渡している。`color-mix()` や相対色の構文が使えるので、派生色(chip の地など)を CSS の側に寄せられる。
4. **明暗の表し方**: 今は Rust 側が配色を丸ごと差し替える。`light-dark()` も使える。
5. **汎用 class を使うか**: Tailwind の文書は「部品ごとの sheet」との併用を勧めていない。使う場合はビルド時に Rust のソースを走査する手順が要り、Node 無しの生成器は encre-css だけ。親や shell が持ち込む口(`provides`)は作ってある。
6. **variant の宣言**: 部品の種類や大きさを「値 → class 名」の表として持つか。Rust では enum から class 名への対応になる。
7. **規則の優先順**: `@layer` で「テーマ / 基礎 / 部品 / 上書き」の順を固定するか。与えられた sheet と自分の sheet の優先の決め方に関わる。
8. **部品の 2 層**: 振る舞い(キー操作、focus、状態)と見た目を分けるか。今の slider、select、color picker は一体。
9. **振る舞いの基準**: 部品のキー操作を APG のパターンに合わせ、テストの基準にするか。
10. **トークンの書式**: DTCG の書式(2025.10)でトークンを持つか。今は Rust の定数と計算。
11. **Stylo の設定**: `:has()` などを有効にするか(`@scope` と同じ扱い)。
