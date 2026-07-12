# gpui-ce drop-in 差し替えスパイク

調査日: 2026-07-12(worker セッションによるスパイク)。目的: Horizon の `gpui`/`gpui_platform` 依存を
community fork [gpui-ce/gpui-ce](https://github.com/gpui-ce/gpui-ce) に `[patch]` で差し替えたときに、
現在の zed pin (`5f8a7413a31769e0882357f90dc424b3962ac72d`) のまま drop-in できるかを実測する。
**このスパイクの Cargo.toml/Cargo.lock 変更は検証専用であり、main には持ち込んでいない**(最終コミットは本ドキュメントのみ)。

---

## 1. 前提

- Horizon のルート `Cargo.toml` は `gpui`/`gpui_platform` を rev 固定なしで
  `git = "https://github.com/zed-industries/zed"` から取得しており、実際のピンは
  `Cargo.lock` の `source = "git+https://github.com/zed-industries/zed#5f8a7413a31769e0882357f90dc424b3962ac72d"`。
  `gpui-component`(longbridge)・`gpui-component-assets` も同様に rev 非固定の git 依存で、
  スパイク実施時点で `0775df394083c1ed74f36f846b78868d1267398f`(2026-07-10 時点の HEAD、GitHub API で確認済み)に解決されていた。
- gpui-ce の README(`https://raw.githubusercontent.com/gpui-ce/gpui-ce/main/README.md`)は
  「upstream の latest を追従する」方針を明言し、"if there's breaking changes and the library
  you're pulling in hasn't updated yet, gpui-ce cannot help you. Otherwise, we treat any mismatches
  as bugs." と書いている ── 本スパイクの結果はまさにこの文の指す状況に一致した(§4)。

## 2. 試した patch ブロック

README が示す「git remote 経由」の最小形はこれ:

```toml
[patch."https://github.com/zed-industries/zed.git"]
gpui = { git = "https://github.com/gpui-ce/gpui-ce" }
```

まずこれに `gpui_platform` を加えた最小構成(タスク指示の「at minimum」)で
`cargo generate-lockfile` を実行し、解決は成功した(968 packages)。ただし解決後の
`Cargo.lock` を確認すると、`gpui_macros` と `gpui_web` が **zed 側と gpui-ce 側の二重ソースで残存**
していた ── `gpui-component` 自身のルート `Cargo.toml` が `gpui`/`gpui_platform` とは別に
`gpui_web`/`gpui_macros`/`reqwest_client` を **直接** `zed-industries/zed` から取っており(下記)、
これらは `gpui`/`gpui_platform` の patch エントリでは対象外になるため:

```toml
# gpui-component 0775df3 の Cargo.toml (該当行のみ)
gpui = { git = "https://github.com/zed-industries/zed" }
gpui_platform = { git = "https://github.com/zed-industries/zed", features = [...] }
gpui_web = { git = "https://github.com/zed-industries/zed" }
gpui_macros = { git = "https://github.com/zed-industries/zed" }
reqwest_client = { git = "https://github.com/zed-industries/zed" }
```

gpui-ce のリポジトリは `gpui_macros`/`gpui_web` を自前クレートとして持っている
(`crates/` 直下に存在を確認済み)ため、これも patch に追加して「公平な」比較にした。
最終的に使った patch ブロックは次の4行(これが本スパイクの実測対象):

```toml
[patch."https://github.com/zed-industries/zed.git"]
gpui = { git = "https://github.com/gpui-ce/gpui-ce" }
gpui_platform = { git = "https://github.com/gpui-ce/gpui-ce" }
gpui_macros = { git = "https://github.com/gpui-ce/gpui-ce" }
gpui_web = { git = "https://github.com/gpui-ce/gpui-ce" }
```

`reqwest_client` は gpui-ce のリポジトリに存在しない(zed 固有の HTTP クライアント実装)ため
patch 対象から除外した。この決定は「gpui-ce が提供しないクレートは無理に patch しない」という
タスク指示に沿う。

**URL 正規化の確認**: `[patch."https://github.com/zed-industries/zed.git"]`(末尾 `.git` あり)は
Horizon 側の依存宣言 `git = "https://github.com/zed-industries/zed"`(末尾 `.git` なし)に対して
warning なしでマッチした ── `cargo generate-lockfile`/`cargo build` のログを `grep -i patch` した限り
"was not used" 等の unused-patch 警告は一度も出ておらず、Cargo のドキュメント通り URL 正規化で
問題なく解決された(実測で確認、ドキュメントの記述を鵜呑みにしていない)。

## 3. 解決された rev / 残存する zed ソース

`cargo generate-lockfile` は2回とも(最小構成・拡張構成いずれも)警告なく成功し、以下に解決された:

- **gpui-ce**: `20340e14874a3b55122e5cb2aa0d023874e08b2d`
  (GitHub API で確認: `Fixes CI and Build issues (#89)`、2026-07-06T02:46:17Z コミット。
  スパイク実施日 2026-07-12 時点で gpui-ce の `main` 最新コミットと一致 ── 6日遅れではなく最新)。
- 拡張 patch 後、`gpui`/`gpui_platform`/`gpui_macros`/`gpui_web` は全て gpui-ce ソースに統一され、
  二重ソースは解消された。
- 一方で `collections`/`derive_refineable`/`gpui_util`/`media`/`perf`/`refineable`/`scheduler`/
  `sum_tree`/`util`/`util_macros`/`zlog`/`ztracing`/`ztracing_macro` は **gpui-ce が別クレートとして
  提供していない**(gpui-ce の `crates/` 一覧に該当なし、GitHub API で確認)ため patch 対象にできず、
  引き続き `zed-industries/zed` から解決された。これらは `gpui-component` が直接依存する
  `reqwest_client`(patch 対象外)の transitive dependency と推測されるが、`cargo tree` での追跡は
  行っておらず **未検証**。ビルド失敗の原因(§4)には無関係だったため深追いしていない。

**手法上の注意点(未検証部分)**: Horizon のルート `Cargo.toml` は `gpui`/`gpui_platform` に rev を
固定していないため、`cargo generate-lockfile` はパッチ対象外の zed ソース(上記の `collections` 等)
も含めて **未固定の git 依存全体を最新 HEAD に再解決**した。実際、`collections` の解決先は元の
pin と同じ `5f8a7413` ではなく `876ec5a8074ba83cce2129ed4d76b59c05a37e9`(zed の 2026-07-12 時点の
より新しい HEAD)になっていた。これは gpui-ce の互換性そのものとは独立した副作用で、後述するように
実際のビルド失敗原因(§4)はこの副作用と無関係だが、「今回の実験＝5f8a7413 ピンとの純粋な1点比較」
ではなく「gpui-ce 側は 5f8a7413→20340e1、非 patch 側は 5f8a7413→876ec5a8 という2つの移動が同時に
起きた状態での比較」だったことは明記しておく。

## 4. 各ステップの結果

### 4a. `cargo build --workspace`(`nice -n 19 cargo build --workspace -j 4`)

**失敗。** ビルド順序は次の通り(ログで確認):

1. `gpui v0.2.2 (gpui-ce#20340e14)` — **成功**(コンパイルエラーなし)
2. `gpui_platform v0.1.0 (gpui-ce#20340e14)` — コンパイル開始(後続のエラーで巻き込まれるが、
   gpui/gpui_platform 自体からのエラーは0件)
3. `gpui-component v0.5.2 (longbridge#0775df39)` — **10件のコンパイルエラーで失敗**
4. `horizon-workspace`(Horizon 自クレート) — `gpui-component` に依存するためここで巻き込まれて停止。
   **Horizon 自身のコードに起因するエラーは0件**(gpui-component が壊れた時点でビルドが止まったため、
   Horizon コードが gpui-ce と噛み合うかどうかはこの回では検証しきれていない)。

エラー内訳(全10件、すべて `gpui-component` 内、すべて `Styled` トレイトの `flex_*` API):

| エラー種別 | 件数 | 対象メソッド |
|---|---|---|
| `E0599`(メソッドが見つからない) | 9 | `flex_grow_1`(7件: `Stateful<E>`×4, `UniformList`×2, `gpui::Div`×1)、`flex_shrink_1`(2件: `Stateful<E>`) |
| `E0061`(引数の数が合わない) | 1 | `flex_grow(width: f32)` として呼んでいるが gpui-ce 版は引数なし |

代表例(`crates/ui/src/input/state.rs:3143`、`gpui-component` 側):

```
error[E0599]: no method named `flex_grow_1` found for struct `Stateful<E>` in the current scope
help: there is a method `flex_grow_0` with a similar name
```

**根本原因の特定**: 3つの `styled.rs` を突き合わせて比較した。

| API | 元の pin (zed `5f8a7413`) | gpui-ce (`20340e1`) | zed 最新 HEAD (`876ec5a8`, 2026-07-12 時点) |
|---|---|---|---|
| `flex_grow` | `fn flex_grow(mut self, grow: f32) -> Self` | `fn flex_grow(mut self) -> Self` | `fn flex_grow(mut self) -> Self`(gpui-ce と同一) |
| `flex_grow_1` | あり | **削除** | **削除**(gpui-ce と同一) |
| `flex_shrink` | `fn flex_shrink(mut self, shrink: f32) -> Self` | `fn flex_shrink(mut self) -> Self` | `fn flex_shrink(mut self) -> Self`(gpui-ce と同一) |
| `flex_shrink_1` | あり | **削除** | **削除**(gpui-ce と同一) |

**重要な発見**: gpui-ce (`20340e1`) の `Styled::flex_grow`/`flex_shrink` シグネチャは、**zed 本家の
最新 HEAD (`876ec5a8`) と完全に一致していた**。つまりこの API 変更は gpui-ce 固有の分岐ではなく、
zed 本家がピン (`5f8a7413`) 以降に行った正規のリネーム/シグネチャ変更を gpui-ce がそのまま追従した
結果である。一方 `gpui-component`(longbridge、スパイク実施日時点の最新コミット `0775df3`、
2026-07-10)は、この zed 本家の変更にまだ追従できておらず、旧シグネチャ(`flex_grow_1`/`flex_shrink_1`/
`flex_grow(f32)`)を呼び続けている。

したがって本質は「gpui-ce が Horizon の pin と噛み合わない」のではなく、
**「`gpui-component` が zed 本家 gpui API の現在地(gpui-ce はそれを正しく追従している)から遅れている」**
という、gpui-ce の README が予告していた失敗モードそのものだった
("if there's breaking changes and the library you're pulling in hasn't updated yet, gpui-ce
cannot help you")。同じ非互換は、gpui-ce を使わずに Horizon 自身の zed pin だけを
`876ec5a8` 相当まで素朴に上げた場合にも(gpui-component 側が追従しない限り)再現すると推測される
──ただしこれは検証しておらず、推測にとどまる。

### 4b. 以降のステップ(`cargo fmt`/`clippy`/`nextest`、`scripts/check-gpui-terminal.sh`)

**未実施。** 4a の `cargo build --workspace` が失敗した時点でプロトコル通り打ち切った。

## 5. エラー分類まとめ(タスク指示の5分類に対して)

- (i) API リネーム/移動: **該当、これが唯一の原因**。`Styled::flex_grow_1`/`flex_shrink_1` の削除、
  `flex_grow`/`flex_shrink` の引数除去。
- (ii) 型の削除/変更: 該当なし(今回観測した範囲では)。
- (iii) クレート構成の不一致: 部分的に該当。gpui-ce は `collections`/`refineable`/`util`/`sum_tree`/
  `zlog`/`ztracing` 等、zed 本家が持つ多数のユーティリティクレートを独立クレートとして提供していない
  (§3)。ただし今回のビルド失敗には無関係だった。
- (iv) gpui-component の非互換: **該当、根本原因**(§4a)。
- (v) Horizon コード自体の非互換: **未検証**(gpui-component で止まったため到達せず)。

## 6. 「今の pin のまま drop-in できるか」への答え

**できない。** `cargo build --workspace` が `gpui-component`(longbridge、Horizon が依存する UI
コンポーネントライブラリ)のコンパイルエラー10件で失敗する。gpui-ce 自体(`gpui`/`gpui_platform`
クレート)は正常にコンパイルでき、gpui-ce の API は zed 本家の最新状態と一致していることも確認した
── 破綻しているのは gpui-ce ではなく、gpui-component が zed 本家 API の現在地に追従できていない点。
gpui-ce は「latest upstream 追従」ポリシー通りに動作しており、README が明言する失敗モード
(依存先ライブラリが追従できていない場合は gpui-ce では救えない)がそのまま発生した。

**検証したこと**: patch 解決の成功(rev 特定含む)、`cargo build --workspace` の失敗と全エラーの
内訳、失敗原因のクレート特定(gpui-component)、API 差分の3点比較(pin / gpui-ce / zed 最新 HEAD)。
**検証していないこと**: gate(`fmt`/`clippy`/`nextest`)、`scripts/check-gpui-terminal.sh`、
Horizon 自身のコードが gpui-ce と噛み合うか(gpui-component で止まったため到達せず)、
残存する zed ソースクレート(§3)の transitive dependency 経路(`cargo tree` 未実行)、
「zed pin だけを素朴に上げても同じ非互換が起きる」という推測(§4a 末尾、未検証)。

## 7. 再評価の条件

次のいずれかが満たされたら再試行する価値がある:

1. `gpui-component`(longbridge)が zed 本家の現在の `Styled::flex_grow`/`flex_shrink` シグネチャ
   (引数なし、`_1`/`_0` variant 整理後)に追従するコミットを出す。README のポリシー通り、
   これが起きれば gpui-ce 側の追加対応は不要なはず。
2. gpui-ce が(README のポリシーに反してでも)旧シグネチャとの互換シムを入れる、または
   逆に Horizon 側で `gpui-component` を fork してこの10箇所を追従させる意思決定をする場合
   (今回のスパイクの範囲外: 「gpui-ce を追いかけて Horizon 側を直す」ことはしない、という
   タスク指示に従い未実施)。
3. Horizon 自身の zed pin を `876ec5a8` 相当以降に更新する計画が別途立ち上がった場合
   ── その際は `gpui-component` の非互換が gpui-ce の有無に関係なく顕在化する可能性が高く、
   本スパイクの §4a がそのまま参考になる。
