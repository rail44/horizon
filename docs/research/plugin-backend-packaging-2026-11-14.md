# 機能をバックエンドごとパッケージにする形の調査(2026-11-14)

## 問い

プラグインの単位を「view と command」ではなく、その機能が動くのに必要なもの全部にしたい。エージェントなら agent の view、その command、`horizon-agentd`、セッションの種類を 1 つのパッケージにまとめ、host はどの機能にも共通の仕組みだけを持つ。

これを実現する形として、他のシステムで何が使われているかを調べた。この文書は観察の記録で、Horizon にどれを採るかの推奨は書かない。

## 出典の確かさの印

調査はサブエージェント 3 体が 2026-11-14 に Web で行い、それぞれの報告をここにまとめた。各項目の印は次のとおり。

- **[V]**: 一次資料(公式文書、仕様、ソースコード、issue)を取得して確かめたもの
- **[S]**: 二次資料(検索結果の要約、第三者のページ)によるもの
- **[M]**: 調査者の記憶によるもので、確かめていないもの

ページに日付や版があるときは併記する。

## Horizon の今の形(コードで確認)

- デーモンは 3 つある。
  - `horizon-agentd`: エージェントのセッションを持ち、shell とは ACP で話す
  - `horizon-terminald`: PTY を持つ
  - `horizon-logd`: ボードのログ
- 起動の仕組みは `crates/horizon-wire/src/spawn.rs` が 3 つに共通で持つ。
  - **起動**: ソケットに接続を試し、繋がらなければ起動して、繋がるまで再試行する。
  - **実行ファイルの探し方**: `HORIZON_<NAME>_BINARY` の環境変数、`horizon` 本体と同じディレクトリ、`PATH` の順。
  - **子プロセスの環境**: `HORIZON_SOCKET` を渡す。
- 版の照合は、shell の側の各クライアントが、最初の呼び出し(`hello` / `initialize`)で行う。合わなければ drain して起動し直す。
- セッションは pane より長く生きる。pane を閉じると detach になり、Horizon を起動し直すと、デーモンが動かし続けているセッションに繋ぎ直す。
- デーモンに固有の知識の多くは host の側にある。
  - **model**: `SessionKind::{Terminal, Agent}`
  - **runtime のクライアント**: `crates/horizon-shell-runtime` が 2 つのデーモンのクライアントを持つ
  - **command**: `Reload Agent Runtime` と `Reload Terminal Runtime` が `CommandId` にある
  - **起動時の復元**: `src/restore.rs` が `SessionInventory { terminal, agent }` でデーモンの在庫と突き合わせる
  - **CLI**: `crates/horizon-cli` の subcommand が固定

## 観察された形

### 形 1: パッケージが起動の方法を記述し、host が起動して持つ

プラグインは「どう起動するか」(コマンド、引数、環境変数)を返すか書くだけで、プロセスは host が起動して所有する。

- **Zed の language server、MCP の context server、デバッグアダプタ**
  - wasm の拡張(`wasm32-wasip2`)が、`language-server-command` / `context-server-command` / `get-dap-binary` で `{command, args, env}` を返す。Zed がそれを実行し、拡張がプロセスを持つことはない。[V] Zed の WIT(`crates/extension_api/wit/since_v0.8.0`)、https://zed.dev/docs/extensions/languages
  - 拡張から使えるプロセスの API は、終わるまで待って出力を集める `process.run-command` だけで、常駐する子プロセスは持てない。[V]
- **VS Code のデバッグアダプタ**: `DebugAdapterDescriptorFactory` が、デバッグのセッションごとに次のどれかを返す。[V] vscode.d.ts
  - `DebugAdapterExecutable`(起動するコマンド)
  - `DebugAdapterServer`(既に動いているポート)
  - `DebugAdapterInlineImplementation`(拡張のプロセス内で動かす)
- **Jupyter の kernelspec**
  - 種類ごとのディレクトリに `kernel.json` を置き、起動のコマンドは `argv` に書く(`{connection_file}` が置き換えられる)。
  - host は検索パスで種類を見つけ、`KernelManager` が起動と監視をする。kernel が死んだら再起動する(`KernelManager.autorestart` が既定で true)。
  - [V] https://jupyter-client.readthedocs.io/en/latest/kernels.html 、https://jupyter-server.readthedocs.io/en/latest/other/full-config.html
- **MCP のクライアント設定**(Claude Desktop の `mcpServers: {name: {command, args, env}}`)と、**ACP の custom agent**(Zed の `agent_servers`、JetBrains の `~/.jetbrains/acp.json`)。
  - クライアントが子プロセスとして起動し、stdio で話す。
  - MCP の stdio の仕様には、「サーバーが予期せず終了したらクライアントは再起動すべき」とある。
  - [V] https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio 、https://zed.dev/docs/ai/external-agents 、https://www.jetbrains.com/help/ai-assistant/acp.html(2026-10-07)

### 形 2: プラグインのコードがプロセスを起動して持つ

- **VS Code の `vscode-languageclient`**(拡張の中で動くライブラリ)が、サーバーを起動して監視する。[V] https://github.com/microsoft/vscode-languageserver-node(client 11.0.0)
  - 起動の仕方は、実行ファイル、Node のモジュール、または関数で指定する。
  - クラッシュしたら再起動する(既定で 4 回まで。3 分以内に 5 回落ちたらやめる)。
  - 止めるときは `shutdown` / `exit` を送り、2 秒後に kill する。
- **Tauri の sidecar**(`bundle.externalBin`)と **Electron の `utilityProcess`** は、アプリが同梱したプロセスを自分で起動する(アプリの単位で、プラグインの単位ではない)。[V] https://v2.tauri.app/develop/sidecar/ 、https://www.electronjs.org/docs/latest/api/utility-process
  - アプリの終了時に sidecar が止まるかは、確かめられなかった。

### 形 3: 宣言だけで、配布はエディタの拡張の外の一覧(registry)

- **ACP Registry**(Zed と JetBrains が共同で発表、2026-01-28)[V] https://zed.dev/blog/acp-registry 、https://github.com/agentclientprotocol/registry
  - エージェントごとに `agent.json` を置く。`distribution` は次のどれか。
    - `binary`: OS と CPU の組ごとに `archive`、`cmd`、`args`、`sha256`
    - `npx` / `uvx`: パッケージの指定
  - CI がスキーマ、URL、版の固定を確かめ、`initialize` が認証の方法を返すことを登録の条件にしている。
  - 版は npm / PyPI / GitHub のリリースから毎時更新される。
  - Zed は v1.5.0 で、拡張としてのエージェント(`extension.toml` の `[agent_servers]`)を非推奨にし、この Registry に移した。インストール済みのものは自動で移行される。[V] https://zed.dev/docs/extensions/agent-servers
  - Zed の MCP の拡張も、MCP Registry へ移す予定とされている。[V] https://zed.dev/docs/extensions/mcp-extensions
- **MCP Registry**(preview): `server.json` のメタデータだけを持ち、パッケージ(npm、PyPI、OCI、mcpb など)を指す。名前空間は GitHub / DNS / HTTP で確かめる。[V] https://modelcontextprotocol.io/registry/about
- **mason.nvim の registry**: `package.yaml` を purl で版まで固定し、mason がそれを取得または build して、実行ファイルを `PATH` に並べる。[V] https://github.com/mason-org/mason-registry
- **Spin のプラグイン**: manifest に OS と CPU ごとの tar の URL と `spinCompatibility` を書く。[V] https://spinframework.dev/v3/plugin-authoring

### 形 4: 1 つにまとめた束(bundle)

- **MCPB**(旧 DXT): 1 つの zip に `manifest.json`、サーバーのコード、依存(`node_modules` や venv)を入れる。
  - manifest の `server.type` は node / python / binary / uv。`mcp_config` に起動の方法を書く。`user_config` の秘密は安全に保存する。
  - 実行時(Node)は host(Claude のデスクトップアプリ)が持つ。
  - [V] https://github.com/modelcontextprotocol/mcpb
- **VS Code のプラットフォーム別 VSIX**(`vsce --target`)。rust-analyzer は、サーバーをプラットフォームごとの VSIX に同梱している。[V] https://code.visualstudio.com/api/working-with-extensions/publishing-extension 、https://rust-analyzer.github.io/book/vs_code.html
- **wasmCloud v1 の provider archive(PAR)**: OS と CPU ごとのネイティブの実行ファイルを、署名付きで 1 つの束にしたもの。v2 で廃止された(形 7)。[V] https://docs.rs/crate/wasmcloud-core/latest/source/src/par.rs

### 形 5: host がプロセスや PTY を持ち、プラグインは id で操作する

- **Zellij の wasm プラグイン**は、自分ではプロセスを起動しない。[V] https://zellij.dev/documentation/plugin-api-commands.html
  - 常駐させたいときは、host に「表示しない command pane」(`open_command_pane_background`)を開かせる。これは Zellij が持つ本物の PTY。
  - プラグインはその pane を id で操作する: 入力を書く、pid を得る、シグナルを送る、もう一度実行する、自分の領域に映す。
  - 終わるまで待つだけなら `run_command` で、結果は 1 回だけ届く。
  - これらの操作には、実行時に利用者が許可する権限(`RunCommands`、`WriteToStdin` など)が要る。
- **Lapce** の wasm プラグインは、`StartLspServer` / `ExecuteProcess` を host(`lapce-proxy`)に依頼する。proxy は、どのプラグインがどの language server を起動したかを記録する。[V] lapce の `lapce-proxy/src/plugin/psp.rs`(v0.4.6)

### 形 6: セッションを持つ長寿命のサーバーに、UI が繋ぎ直す

- **VS Code の pty host**: シェルのプロセスは、ウィンドウとは別のプロセスが `persistentProcessId` ごとに持つ。[V] https://code.visualstudio.com/docs/terminal/advanced
  - ウィンドウを読み込み直したときは、そのプロセスに繋ぎ直す。60 秒以内に繋ぎ直されなければ止める。[V] vscode の `ptyService.ts` など
  - アプリを起動し直したときは、表示していた内容を戻し、同じ環境でプロセスを起動し直す(revive)。
  - 拡張が作る terminal(Pseudoterminal)はこの仕組みに乗らず、保存されない。[V] `extHostTerminalService.ts`
- **tmux、zellij、Emacs の daemon、Neovim の `--listen` / `--embed`**: サーバーが全セッションを持ち、クライアントは attach / detach / list / kill する。[V] https://man.openbsd.org/tmux.1 、https://www.gnu.org/software/emacs/manual/html_node/emacs/Emacs-Server.html 、https://neovim.io/doc/user/api-ui-events/
  - Neovim は複数の UI を同時に繋げられ、その能力は全 UI が求めたものの共通部分になる。
- **Jupyter Server**: kernel はサーバーのプロセスの下にあり、ブラウザを読み込み直すと繋ぎ直す。繋がっていない間のメッセージは `session_id` ごとに溜めておき、繋ぎ直したときに送る。[V] https://jupyter-server.readthedocs.io/en/latest/api/jupyter_server.services.kernels.html
- **ACP**: セッションはエージェントの中にある。[V] https://agentclientprotocol.com/protocol/session-setup 、https://agentclientprotocol.com/protocol/session-list
  - `session/load`: 履歴を全部送り直す
  - `session/resume`: 送り直さない
  - `session/list`、`session/close`
  - いずれも capability で可否を示す。安定版か草案かは、仕様のページからは確かめられなかった。
- **バックエンドの再起動をまたいで残すもの**
  - Jupyter Enterprise Gateway: 起動の引数と接続の情報を保存し、ゲートウェイを起動し直したあと kernel に繋ぎ直す(実験的)。[V]
  - zellij: レイアウトとコマンドを保存し、起動し直したときにコマンドを走らせ直す(プロセスそのものは残らない)。[V] https://zellij.dev/documentation/session-resurrection.html

### 形 7: wasm そのものをバックエンドにする、または host に組み込んだ能力を WIT で渡す

- **wasmCloud v2 の services**: 1 つの長寿命の instance が workload の間ずっと動く。[V] https://wasmcloud.com/docs/overview/workloads/services/
  - trap したら、決まった回数の範囲で instance を作り直す。
  - TCP で待ち受けられ、component からは localhost で繋ぐ。
  - 1 つの workload に 1 つまで。
- **wasmCloud v2 の host plugins**: Rust の `HostPlugin` trait を host の実行ファイルに組み込み、その能力を component に WIT で渡す。[V] https://wasmcloud.com/docs/overview/hosts/plugins/ 、https://wasmcloud.com/docs/migration/(2026-09-01)
  - v1 の「外部の capability provider」(ネイティブのプロセスが NATS で話す形)は、v2 で意図的に廃止された。
- **Spin 4.0**(2026-06-15): WASIp3 を使う。1 つの instance が複数の要求を同時に扱え、`wasip3::spawn` で `handle` より長く生きる background task を作れる。[V] https://spinframework.dev/blog/announcing-spin-4-0
- **wasm でできないこと**
  - **プロセスの起動**: WASI には、どの段階にも提案がない。
    - issue は #414(2021)と #899 `wasi:exec`(2026-03-07)が開いているだけ。
    - #899 では、任意のホストプロセスの起動は「WASI でいうサンドボックスからの脱出に等しい」という指摘がある。
    - [V] https://github.com/WebAssembly/WASI/blob/main/docs/Proposals.md
  - **PTY**: `wasi:cli` の `terminal-input` / `terminal-output` にメソッドがない(0.2、0.3 とも)。[V]
  - **Unix ドメインソケット**: `wasi:sockets@0.3.0` は ipv4 / ipv6 だけ。[V]
  - **ネイティブのライブラリ**(DuckDB など): component は WIT の import しか使えない。[M]
  - **スレッド**: WASI threads は Phase 1。[V]
  - WASI 0.3.0 は 2026-06-11 に出て、wasmtime 46 以降で既定で有効。[V] https://bytecodealliance.org/articles/WASI-0.3
  - Wasmer の WASIX(WASI とは別の仕様)は、プロセスの起動と TTY を足している。[S]
- **プロセスの境界**: wasmtime の resource の handle は store に属し、その外では意味を持たない。[V] https://docs.rs/wasmtime/latest/wasmtime/component/struct.Resource.html
  - プロセスをまたぐ RPC の wRPC は WIT を運ぶが、resource は中身の分からないバイト列として扱う。[V] https://github.com/bytecodealliance/wrpc(v0.18.0、2026-10-23)

### 形 8: 起動の場所と方法を差し替える層

- **Jupyter の kernel provisioner**(jupyter_client 7.0 以降): kernelspec の `metadata.kernel_provisioner` で、ローカル、K8s、YARN などの起動の方法を選ぶ。[V] https://jupyter-client.readthedocs.io/en/latest/provisioning.html
  - provisioner は `subprocess.Popen` に倣った口(`launch_kernel`、`poll`、`kill`、`terminate`、`cleanup`)を持つ。
  - `get/load_provisioner_info` で、別のプロセスから繋ぎ直すための情報を保存できる。
- **VS Code の `extensionKind`**(ui / workspace)と remote: workspace 側の拡張と、その子プロセスはリモートで動く。[V] https://code.visualstudio.com/api/advanced-topics/remote-extensions
  - JetBrains の split mode も、プラグインを frontend / backend / shared のモジュールに分ける。[V] https://plugins.jetbrains.com/docs/intellij/split-mode-and-remote-development.html(2026-11-10)

## 横断する論点

### 実行ファイルの入手

次の 4 通りが並んで使われている。

- **パッケージに同梱**: VS Code のプラットフォーム別 VSIX、Tauri、MCPB
- **初回に取得**: Zed の拡張のコード、vscode-clangd、Lapce
- **`PATH` にあるものを使う**: Helix、Neovim。Zed と clangd も最初に `PATH` を見る
- **一覧(registry)から host が取得**: ACP Registry。JetBrains は Node / Python の実行時まで用意する

### 版の照合

5 通りが見られた。

- **完全一致でなければ繋がない**: tmux の `PROTOCOL_VERSION`、VS Code Server(クライアントの commit ごとに入れる)、WezTerm の mux、Lapce の proxy(版が違えば取得し直す)
- **契約の版でソケットの名前空間を分ける**: zellij 0.44.0 以降の `contract_version_1`。同じ契約の版なら、異なる zellij の版どうしで attach できる。[V] https://zellij.dev/news/remote-sessions-windows-cli/
- **メジャーの整数 1 つ。合わなければ切断すべき**: ACP の `protocolVersion`
- **日付の版を要求ごとに付け、合わなければ対応する版の一覧を返して再試行する**: MCP の 2026-07-28 版。`initialize` の handshake と、プロトコル上のセッションを廃止した。[V] https://modelcontextprotocol.io/specification/2026-07-28/changelog
- **版を持たず capability の flag だけ**: LSP 3.x、DAP。Neovim は `api_level` と関数ごとの `since` を持つ

### 信頼

調べた範囲では、プラグインが持ち込むネイティブのバックエンドをサンドボックスに入れているシステムはない。代わりにあるのは次のもの。

- **プラグインの API 呼び出しを権限で制限**: Zed の `process:exec` / `download_file` / `npm:install`、Zellij の実行時の許可、Tauri の shell の権限
- **チェックサムと名前空間の確認**: ACP の `sha256`、MCPB の `fileSha256`、MCP の名前空間
- **配布元での審査と署名**: VS Code Marketplace
- **OS のインストーラに任せる**: ブラウザの native messaging。拡張とネイティブ側の両方が相手を許可する
- **接続ごとの秘密**: Jupyter の HMAC の鍵、VS Code Server の乱数の鍵

### プラグインどうしの依存

- **VS Code**: `extensionDependencies` と、`activate()` の戻り値(`exports`)で API を渡す。別の host にいる拡張どうしは、JSON の引数の command で話す。[V]
- **JetBrains**: `<depends>` と `<extensionPoints>`。[V]
- **Zed、Zellij、wasmCloud v2**: 宣言された依存はない。
  - Zellij の pipe は、宛先のプラグインが動いていなければ起動して届ける。[V]
  - wasmCloud v2 の service へは、localhost の TCP で繋ぐ。

## Horizon の 3 つのデーモンと、上の制約の対応(観察)

- **`horizon-terminald`**: PTY を持つ。WASI には PTY もプロセスの起動もないので、wasm の component だけでは書けない。
  - 前例: VS Code の pty host、Zellij の command pane は、どちらも host か別のネイティブプロセスが PTY を持つ形。
- **`horizon-agentd`**: DuckDB(ネイティブのライブラリ)を使い、bash のツールでプロセスを起動する。どちらも WASI の外にある。
  - ACP では、エージェントは別のプロセスとして配られる(Registry の `binary` / `npx` / `uvx`)。
- **`horizon-logd`**: ボードのログを持つ。この調査では、wasm で書けない理由は見つけていない(未調査)。
- **Horizon がすでに持つもの**: 3 つのデーモンに共通の起動の仕組み(`spawn.rs`)は、形 1 の「host が起動して持つ」側にあたる。`horizon-agentd` と `horizon-terminald` の版の照合(`HORIZON_ACP_EXT_VERSION` の比較、terminal の `MIN_SUPPORTED == current`)は「完全一致」の型にあたる。
