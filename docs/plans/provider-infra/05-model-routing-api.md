# Plan 05 — モデルルーティング API(OpenAI 互換)

Status: ready

## 背景

複数モデルを組み合わせた OpenAI 互換 API を手元に用意する(所有者の
構想。上流には synthetic.new を選定済み — 多モデルを1契約で提供)。
Horizon 側は base_url を差し替えるだけの消費者で居られるため、ルーター
自体は再利用可能な独立資産として作る。「このリポジトリに同居させ、
horizon 系クレートへ依存させず、後日そのまま切り出せる構造」は決定済み
(docs/roadmap.md「Later」)。

## スコープ

- 新ワークスペースメンバー(クレート名はドメインで決定。例:
  `crates/horizon-router`)。**horizon 系クレートへの依存ゼロ**を構造で
  担保(切り出し可能性の条件)。
- OpenAI 互換の `/chat/completions` を受け、規則に従って上流
  (synthetic.new)のモデルへ振り分ける。**ストリーミング(SSE)対応は
  必須**(horizon-agent が前提にしている)。
- 秘密情報は環境変数のみ(このリポジトリの既存方針)。

## 完了条件

- horizon-agent の `base_url` をルーターへ向けて、実際のエージェント
  対話(ツール呼び出し込み)が一連で通る。
- ルーターが horizon 非依存であることがビルド構造で確認できる。
- ゲート緑。

## ドメインセッションに委ねる設計判断

- ルーティング戦略の v1 範囲(固定エイリアス表か、役割/コスト規則か)。
- 設定の形式と置き場所、常駐の形(手動起動か Horizon が spawn するか)。
- クレート名・API 面の細部。

## 参照

docs/roadmap.md(Later)/ docs/research/agent-prompting.md(synthetic.new
の実測情報)/ docs/cli-control-plane-design.md(同居・独立クレートの前例)
