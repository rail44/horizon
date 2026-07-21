# crush / opencode ツール群調査（2026-07-07、トランスクリプトから回収）

> 2026-07-07 のプロジェクトセッションで行われた調査の本文。doc 化されずセッション
> トランスクリプトにのみ残っていたものを、backlog 18/19（web 検索・公開コード
> 検索）の相談再開に伴い 2026-07-19 に回収・結晶化した。調査時点のリポジトリ
> 状態は crush @ d341d84b24fb（2026-07-06 HEAD）、opencode dev ブランチ
> （2026-07-07 時点）。以後のドリフトがありうる。

## 経緯

オーナーが元々求めていた「search tool」の中身を詰める相談の中で、2 つの
サブエージェントを並行起動し、`charmbracelet/crush`（Go 製）と `sst/opencode`
（TypeScript 製）それぞれの検索系ツール実装を読み取り専用で調査させた。目的は
Horizon の `fs.grep`/`fs.glob` と対比し、web 検索ツールを追加する際の設計材料に
すること。

## opencode（sst/opencode）側の報告

### 調査対象の確認

`gh api repos/sst/opencode` は `anomalyco/opencode` にリダイレクトされる
（fork: false、同一リポジトリ）。デフォルトブランチ `dev`。

構造上の注意: 2 つの並行ツール実装群 — `packages/opencode/src/tool/*`（現行、
CLI/セッションランナーに実際に配線）と `packages/core/src/tool/*`（v2
ToolRegistry への移行途中、未完成）。前者を主対象とした。

### 1. ツールの棚卸し（検索・探索系）

| 名前 | ファイル | 説明 | 返すもの |
|---|---|---|---|
| grep | tool/grep.ts + grep.txt | 正規表現コンテンツ検索、include でファイルパターン絞込み | パス+行番号+マッチ行 |
| glob | tool/glob.ts + glob.txt | glob パターンによるファイル名検索 | マッチした絶対パス一覧 |
| read | tool/read.ts + read.txt | ファイル読み取り（ディレクトリならエントリ一覧、専用 ls ツールなし） | 行番号付きテキスト/ディレクトリエントリ/画像・PDF 添付 |
| lsp | tool/lsp.ts + lsp.txt | goToDefinition/findReferences/hover/documentSymbol 等 | 各 LSP レスポンスの生 JSON |
| webfetch | tool/webfetch.ts + webfetch.txt | 単一 URL 取得、markdown/text/html 指定 | 変換済みコンテンツ or 画像添付 |
| websearch | tool/websearch.ts + websearch.txt | Web 検索（Exa/Parallel） | 検索結果テキスト（上流そのまま） |

grep.txt/glob.txt には「複数ラウンドの探索が必要なら Task ツール（サブ
エージェント委譲）を使え」との誘導が明記。embedding/semantic search ツールは
repo 全体を grep しても該当なし（存在しない）。

### 2. 実装方式

- **grep/glob**: どちらも `packages/core/src/ripgrep.ts` の Ripgrep.Service 経由で
  rg バイナリに shell out。grep は `rg --json ...` をストリームで 1 行ずつパースし
  Schema 検証。glob は `rg --files --glob=<pattern> --glob='!**/.git/**' .`。
- **ripgrep バイナリの入手**（`packages/core/src/ripgrep/binary.ts`）: npm 同梱では
  ない。優先順位は ① PATH 上の rg（which）② キャッシュ済み Global.Path.bin/rg
  ③ どちらもなければ GitHub Releases（BurntSushi/ripgrep、バージョン固定 15.1.0）
  から実行時ダウンロードし tar.gz/zip 展開・chmod 755・キャッシュ保存。
- **lsp**: `packages/opencode/src/lsp/*` の LSP.Service 経由で実際の言語サーバ
  プロセスを起動。read.ts は読み込み時に lsp.touchFile() をバックグラウンド Fiber
  で叩く（ウォームアップのみ、結果は返さない）。重要: registry.ts で `lsp`
  ツールは `flags.experimentalLspTool` が true の時のみビルトインリストに挿入され、
  デフォルト false。LSP ベースのシンボル検索はデフォルト非公開の実験的機能。
- **webfetch**: 自前実装。Effect の HttpClient で HTTP 取得、htmlparser2（テキスト
  抽出）と turndown（HTML→Markdown 変換）。検索エンジンではなく単一 URL フェッチ
  のみ。Cloudflare の bot 検知（cf-mitigated: challenge）検知時 UA 変更でリトライ。
- **websearch**: 自社検索実装ではなく、**外部リモート MCP エンドポイントを素の
  HTTP POST + 自前の軽量 JSON-RPC クライアント**（tool/mcp-websearch.ts）で叩く。
  フル MCP クライアント SDK は未使用。

### 3. 結果の整形

- grep: 最大 100 件、1 マッチのテキストは 2000 文字に切詰め、submatches 最大
  100 件。`Found N matches (more matches available)` ヘッダ後ファイルパスごとに
  グループ化、`Line N: text` 形式、絶対パスに解決。
- glob: 最大 100 件、絶対パスのフラットリスト、ソートなし。
- read: デフォルト 2000 行 / 最大 50KB 上限、1 行 2000 文字超は切詰め。バイナリ
  拒否、画像/PDF は base64 データ URL 添付。
- lsp: 整形せず `JSON.stringify(result, null, 2)` の生出力。
- webfetch: 5MB 上限、タイムアウトデフォルト 30 秒・最大 120 秒。
- **websearch: 上流（Exa/Parallel）の MCP レスポンスの content[0].text をほぼ
  そのまま返す（整形はプロバイダ委任）。**

### 4. 権限・安全

- 全ツールが `ctx.ask({permission: ...})` で Permission Service を通す。最終一致
  優先のワイルドカードマッチ（allow/ask/deny）。公式ドキュメントによれば
  「ほとんどは allow」「read は .env 系のみ deny」「doom_loop と
  external_directory は ask がデフォルト」。--auto 起動時は ask 対象を自動承認。
- workspace 外アクセス制限: `tool/external-directory.ts` が対象パスが現在の
  worktree 配下か判定、範囲外なら external_directory permission（デフォルト ask）
  で別途承認要求。
- gitignore 尊重: ripgrep の標準動作がそのまま適用。`.git/` だけ明示除外。

### 5. Web 検索の実装（プロバイダ選択の要点）

- プロバイダは **Exa**（`https://mcp.exa.ai/mcp`、ツール名 `web_search_exa`）と
  **Parallel**（`https://search.parallel.ai/mcp`、ツール名 `web_search`）の 2 択。
  `selectWebSearchProvider` がセッション ID の checksum で概ね 50/50 に振り分け、
  環境変数（OPENCODE_WEBSEARCH_PROVIDER / OPENCODE_ENABLE_EXA /
  OPENCODE_ENABLE_PARALLEL）で上書き可能。
- 認証は EXA_API_KEY（URL クエリ埋め込み）、PARALLEL_API_KEY（Bearer ヘッダ）。
- `webSearchEnabled()` により、opencode 自社ホスト型プロバイダ利用時は常時有効。
  それ以外（自前 API キー利用）では enable フラグが立っていない限り websearch
  ツール自体が LLM に提示されない（デフォルト無効。推測=未確認: 自社ゲートウェイ
  側で Exa/Parallel キーを仲介しているためと思われる）。

## crush（charmbracelet/crush）側の報告

調査対象: charmbracelet/crush main ブランチ HEAD d341d84b24fb（2026-07-06 時点）。
ツール定義本体は `internal/agent/tools/`、ツール一覧の組み立ては
`internal/agent/coordinator.go`（buildTools）。

### 1. ツール棚卸し（検索・探索系）

| 名前 | 説明（md.tpl の実文言、要旨） | 主パラメータ | 返すもの |
|---|---|---|---|
| grep | regex/リテラル検索、mtime 順最大 100 件、.gitignore 尊重 | pattern(必須), path, include, literal_text | ファイルごとに `Line N, Char C: ...` |
| glob | ファイル名パターン検索、mtime 順最大 100 件、隠しファイルスキップ | pattern(必須), path | 改行区切りパス一覧 |
| ls | ツリー表示、最大 1000 件 | path, ignore, depth | インデント付きツリー |
| sourcegraph | Sourcegraph 経由で公開 GitHub を横断検索（regex/言語/リポジトリ/シンボル、最大 20 件） | query, count(≤20), context_window(既定 10 行), timeout(≤120s) | リポジトリ/ファイル単位のコードスニペット |
| lsp_references | LSP でシンボル参照検索 | symbol(必須), path | ファイルごとの Line L, Column C |
| web_search | DuckDuckGo 検索、title/URL/snippet | query(必須), max_results(既定 10, 最大 20) | 番号付き Title/URL/Summary |

隣接: web_fetch（URL→Markdown 化、大きいページは一時ファイル保存）、fetch
（汎用 URL 取得、要パーミッション）、**agentic_fetch（検索/取得を丸ごとサブ
エージェントに委譲）**、view（ファイル読み取り）。embedding/semantic search 系は
存在しない。

### 2. 実装方式

- **grep**（grep.go, rg.go）: 第一選択は rg バイナリへの shell out（PATH 依存、
  同梱なし）。`rg --json -H -n -0 <pattern> [--glob include] <path>`、
  --ignore-file .gitignore/.crushignore（存在すれば）付与。rg が無ければ Go 製
  フォールバック（標準 regexp + fastwalk + 自前 gitignore 判定）。
- **glob**（glob.go, rg.go）: `rg --files --null [--glob pattern]` が第一選択。
  シンボリックリンク追跡（-L）は意図的に不使用（$HOME や nix ストアへの脱出・
  循環によるハング防止、とコメント明記）。失敗時は doublestar/v4 + fastwalk に
  フォールバック。
- **ls**: rg 不使用、純 Go。go-git の gitignore 実装で階層的 ignore 判定
  （.gitignore/.crushignore + git core.excludesFile + crush 独自グローバル +
  node_modules/.git/dist/lockfile 等のハードコード除外）。
- **sourcegraph**（sourcegraph.go）: HTTP クライアントのみ。
  `https://sourcegraph.com/.api/graphql` へ GraphQL POST。**API キー無し・公開
  エンドポイント、パブリックリポジトリのみ検索。**
- **lsp_references**（references.go）: grep 実装でシンボル出現行を先に絞り込み、
  その座標を実 LSP クライアントの FindReferences に渡す 2 段構成。
- **web_search**（web_search.go, search.go）: 公式 API ではなく
  `https://lite.duckduckgo.com/lite/?q=...` の **HTML スクレイピング**。
  golang.org/x/net/html で DOM を歩き result-link/result-snippet を抽出。
  ブロック回避のため User-Agent（11 種）/Accept-Language をランダム
  ローテーション、呼び出し間隔を 500–2000ms でジッタ遅延。

### 3. 結果整形

- grep: `Found N matches` 後、ファイルごとに `Line L, Char C: <text>`。表示幅
  500 カラムに切詰め。mtime 降順・上限 100 件（ハードコード）。件数・truncated
  はメタデータとして本文と別添付。
- sourcegraph: API が返す file.content から context_window 行分の前後文脈を
  コードブロック整形。実装不整合あり: params.Count は最大 20 を受けるが整形段階で
  無条件に上位 10 件にハードカット。
- web_search: 番号付き Title/URL/Summary。0 件時は "No results found. Try
  rephrasing your search."

### 4. 権限・安全性

- grep/glob/sourcegraph/lsp_references は permission.Service を一切受け取らず、
  承認プロンプトなし。grep/glob は path をワークスペース外制限なしに探索ルートに
  使う（ls/view にある「ワーキングディレクトリ外なら permissions.Request」の
  チェックが無い）。安全策は timeout・件数上限・シンボリックリンク非追跡のみで、
  コメント上も「ハング/CPU 占有対策であって機密境界ではない」設計。
- gitignore の尊重度は経路により差: rg 経由はルート直下の --ignore-file のみ、
  Go フォールバックと ls は階層的マッチャ — 厳密さが経路で異なる（確認済み）。
- 全ツール横断の PreToolUse フック層（hooked_tool.go）がトップレベルの全ツール
  呼び出しをラップ（ユーザー設定シェルフックで deny/allow/halt/入力書換）。
  ただしサブエージェント内には発火しない。
- **web_search/web_fetch はコード中コメントで明示的に "no permissions needed" と
  され、トップレベルのコーダーには直接公開されない。agentic_fetch ツール
  （agentic_fetch_tool.go）が生成する使い捨てサブエージェントの内部ツールセット
  としてのみ存在。ゲートは外側の agentic_fetch 呼び出し自体への
  permissions.Request（Action: "fetch"）のみで、承認後は AutoApproveSession に
  よりサブセッション内の web_search/web_fetch/glob/grep/view は無承認で連鎖
  実行される。**
- 検索特化の task サブエージェントは {glob, grep, ls, sourcegraph, view} のみに
  限定（lsp_references・agentic_fetch・agent 自体は含まれず再帰不可）、
  AllowedMCP は既定で空。

### 5. Web 検索プロバイダ

DuckDuckGo Lite（lite.duckduckgo.com）の非公式 HTML スクレイピング。公式 API・
API キーは使用せず（他エンジンの統合も無し）。GitHub 固有クエリの場合は gh CLI
併用を促す文言がツール説明に動的追加される（gh が PATH にある場合のみ）。

## 当時の統合コメント（Horizon との 3 者比較）

コード検索（grep/glob）の実装方式は 3 者で割れている:

- crush（Go）: rg に shell out、無ければ Go 製フォールバックの二段構え。
- opencode（TS）: 常に ripgrep に shell out。バイナリは PATH → キャッシュ →
  GitHub Releases から実行時ダウンロードの三段構え。
- Horizon: 完全に自前（walkdir + regex + globset、外部バイナリ依存ゼロ）。

Horizon の自前路線は思想に整合（ネイティブ Rust、外部バイナリ調達の不確実性
ゼロ）。crush/opencode の「rg に投げる」は彼らが薄いプロセスだから自然なだけで、
Horizon が真似する理由は薄い。差分は 2 点のみ:

1. **gitignore 非尊重**: Horizon の traverse はハードコード除外セットだけで
   リポジトリの .gitignore を見ていない（埋める価値のある唯一の実質ギャップ）。
2. **「外に開いた検索」の系統が無い**: crush は sourcegraph + web_search +
   lsp_references、opencode は websearch + webfetch + lsp（既定オフ）を持つ。

設計として注目は crush の **agentic_fetch**: web 検索/取得をトップレベルには
直接見せず、使い捨てサブエージェントの内部ツールにして、外側の 1 回の承認だけで
ゲートする。Horizon の委譲・スキル機構に思想が近い。

「search tool を持たせたい」は 3 つに分かれ、打ち手が全く違う:

1. コードベース検索の強化 → 既にある。やるなら gitignore 尊重の追加だけ。
2. Web 検索 → 新規ツール。プロバイダ選択が論点（DDG スクレイピング=無料だが
   脆い / Exa・Parallel=API キー要 / 素直な検索 API）。信頼境界と承認設計も要る。
3. 公開コード検索（Sourcegraph 型）や LSP シンボル検索 → また別物。

この整理が backlog 18（= 2）と 19（= 3）になった。

## 2026-07-21 addendum: Sourcegraph integration route

上記の crush 調査時点では Sourcegraph GraphQL を実装例として記録したが、現行の
Sourcegraph 7 公式文書は GraphQL を互換保証のない内部 debug API と位置づける。
外部のコード検索統合向けに現在案内されているのは
`GET https://sourcegraph.com/.api/search/stream` の Streaming Search API で、
Sourcegraph.com の公開コードは認証なしで検索できる。匿名の quota/SLA は公開
されていないため、Horizon はこれを唯一の保証 backend ではなく、固定 endpoint・
上限付き・自動 retry なしの best-effort adapter として採用する。

Horizon の `public_code_search` は model 入力から件数・timeout・visibility・type を
上書きさせず、boolean/grouping も v1 では拒否したうえで、括った検索式に public
GitHub/file search の trusted constraints を AND 適用する。SSE の raw bytes、
event/field/snippet、normalized output、全体時間、同時実行数と開始間隔を再制限し、
repository・path・commit の帰属情報を保持する。LSP references は remote public
search と異なり、language-server の発見・起動・workspace ownership・document sync
を必要とするため別 backlog のまま残す。

Primary references:

- <https://sourcegraph.com/docs/api>
- <https://sourcegraph.com/docs/api/stream-api>
- <https://sourcegraph.com/docs/code-search/queries>
