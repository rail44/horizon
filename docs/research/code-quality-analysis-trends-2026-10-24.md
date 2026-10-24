# コード品質解析手法の動向 2026(2026-10-24 調査)

オーナーの依頼(2026-10-24)は「refactor auditの仕組みをよりよくしたいです。コード品質についての解析手法の現在のトレンドや研究についてまず調査してもらえると。」である。対象の監査ツールは `scripts/refactor-audit` で、その構成(rust-code-analysis 0.0.25 による関数単位の LOC・循環的・cognitive 複雑度、jscpd 5.3.1 の `--mode weak --min-tokens 70` を識別子保持と正規化の 2 回、ast-grep 0.44.1 の限定ルール、任意で Code Maat 1.0.4 の共変更結合と cargo-modules 0.26.0 の依存グラフ、人間/エージェントによる refactor/preserve 判定の台帳)は、調査を依頼したセッションが `scripts/refactor-audit/README.md` と `tool-versions.json` から取って調査担当に渡したものである。方法は、Opus の調査エージェント 5 体による並行の Web 調査で、各エージェントが 1 領域(静的メトリクスの妥当性 / クローン検出 / 履歴・アーキテクチャ解析 / LLM 併用 / Rust ツール群と振る舞い保存)を担当した。検証の表記は次のとおりである。★ は調査者が一次資料を直接読んだ主張を示す。ただし多くは abstract ページか HTML 版だけを読んだもので、その場合は「★(abstract)」のように明記する。無印の主張は検索スニペットか二次的な要約に由来し、一次資料とは照合していない。「[prior knowledge]」は、調査者が今回再取得せずに挙げた古い論文を示す。生の調査ノートはジョブの一時ディレクトリ(`/home/satoshi/.claude/jobs/da8abac5/tmp/research_notes/コード品質解析手法の動向 2026/`)にあり、揮発する。恒久的な記録はこのレポートである。

各調査者の検索予算は 15〜25 回程度に限られていた。「見つからなかった」は「存在しない」の証明ではない。

## 主要な知見

- **Rust で妥当性を検証した静的メトリクス・スメルの研究は一件も見つからなかった。** 証拠のほぼすべては Java で、一部が C/C++・C#・Python・JS である。Rust 関連の既存研究は記述的な言語間比較(Ardito et al., PeerJ CS 2021)、Clippy の利用実態(arXiv 2310.11738)、C→Rust 翻訳品質(arXiv 2602.00840)にとどまる。
- **cognitive complexity は理解時間とは r = 0.54 で相関するが、正しさとは r = −0.13 である。** 出典は Muñoz Barón et al., ESEM 2020, arXiv:2007.12520 で、★ HTML を読んだ。10 データセット・427 スニペットの大半は Java である。従来メトリクスを上回る改善はない(Lavazza et al., JSS 2023, 無印)。妥当性を確かめた閾値は 15 を含めて存在しない。
- **欠陥予測では、プロセス(履歴)メトリクスがプロダクト(静的)メトリクスを大きく上回る。** 中央値 AUC は 0.95 対 0.54、中央値 recall は 98% 対 44% である。出典は Majumder, Mody & Menzies, EMSE 27(3) 2022, arXiv:2008.09569 で、★(abstract)、GitHub 700 プロジェクト・722,471 コミットで測られた。ただし同一研究グループのパイプラインによる結果で、独立追試は見つからず、差がほとんどないとする反対例もある(Stanić & Afzal, QRS-C 2017, 無印)。
- **BigCloneBench の WT3/T4 ラベルは 93.3% が誤りである。** 406 ペアの層化標本で確かめられた(Krinke & Ragkhitwetsagul, arXiv:2505.04311, 2025, ★ HTML, Java)。BigCloneBench を使った 179 本の論文のうち 77.7% が、この誤ったラベルで意味的クローンを評価している。ML/LLM による「意味的クローン検出」の高い F1 値の多くは証拠にならない。
- **Rust の Type-1/2 クローンで最も多いのは、型ごとにコピーされたトレイト実装である。** 15 crate・3,033 クローンを手作業で分類した結果による(Pizzolotto et al., IPSJ 技報 2022, 査読なし, ★)。HIR 上でマクロを展開すると、報告は「真陽性だがリファクタリングには無用」なものだらけになる。cgmath では 328 件が 165,834 件に増えた。
- **クローンの害を予測する指標として支持があるのは、共変更・遅延伝播・既存の不整合・開発者の認識である。長さは支持されない。** Juergens et al. ICSE 2009 では、意図しない不整合の約 50% が欠陥だった(C#/COBOL/Java, ★)。Wagner et al. 2016 では、長さと欠陥の相関は ρ = 0.268, p = 0.12 で有意でなかった(産業 Java, ★)。Type-3 クローンの害については研究間で結論が対立している。
- **CodeScene の Code Health/ホットスポットの定量的証拠は、すべて CodeScene 社員が著者である。** 「15 倍の欠陥」は arXiv:2203.04374, TechDebt 2022, ★(abstract) による。2024 年の回帰は R² = 0.038(欠陥)、0.006(時間)で、ファイルサイズと churn を統制していない(arXiv:2401.13407, ★ HTML, 主に C++/C#)。独立の追試は見つからなかった。
- **LLM の生のリファクタリングは高率で振る舞いを壊す。** StarCoder2 のテスト通過率は pass@1 28.36% だった(arXiv 2411.02320, Java, 無印)。Extract Method の提案は最大 62.3% が hallucination だった(arXiv 2401.15298, Java/Kotlin, 無印)。正しさは IDE・コンパイラ・テスト・検証器という外部フィルタで回復されている。
- **LLM を品質の判定者に使うと、本来関係のない摂動で判定が変わる。** 応答の順序、変数名、誤導的コメントの影響を受ける(CodeJudgeBench, arXiv 2507.10535, 無印)。readability での人手評価との一致は Pearson r = 0.25(Java)、0.02(Python)、0.00(CUDA)である(CoReEval, arXiv 2510.16579, 無印)。コード品質判定を繰り返したときの安定性の数値は見つからなかった。
- **rust-code-analysis は archive されていないが、実質休止している。** 最終リリースは 0.0.25(2023-01-13)、master の最終コミットは 2026-01-20 である。2026-10 の issue #1249 は、`match`・`loop`・`let … else`・`&&`/`||` 混在で McCabe と SonarSource 仕様からの逸脱を報告している(★, 2026-10-24 確認)。Clippy 自身も `cognitive_complexity` を `restriction` に置き、測定に使わないよう注記している(★)。
- **Rust で使えるリファクタリング検出器は存在しない。** RefactoringMiner 3.1.6 は Java/Python/Kotlin/C/C++ に対応するが Rust は未対応で、RefDiff は 2021 年以降停止している(★)。Rust 固有のリファクタリング等価性の研究として見つかったのは REM 2.0(arXiv 2601.19207, ★(abstract))だけで、対応するのは Rust の部分集合に限られる。

## 静的メトリクスの妥当性

### cognitive complexity は時間を予測するが、正しさや欠陥はほとんど予測しない

| 研究 | データ・言語 | 結果 | ★ |
|---|---|---|---|
| Muñoz Barón, Wyrich & Wagner, ESEM 2020, arXiv:2007.12520 | 既存 10 研究、427 スニペット、評価約 24,000 件。主に Java、ほか C/C++・Scala・C#・JS | 理解時間 r = 0.54(9 研究)、正しさ r = −0.13、主観評価 r = −0.29、fMRI r = 0.00(1 研究 12 スニペット)、時間と正しさの合成 r = 0.40。信頼区間は報告されていない | ★ ar5iv HTML |
| Lavazza, Abualkishik, Liu & Morasca, JSS 197:111561 (2023) | 既存の理解性データセット、SVR | 従来メトリクスと「ほぼ同程度」の相関で、「有意な改善になっていない」 | 無印(OSF preprint) |
| Lenarduzzi, Kilamo & Janes, arXiv:2303.07722 | 経験 1〜4 年の開発者 216 名、Java 12 クラス | CyC・CoC とも知覚的理解性の「控えめな予測子」。値が高いと理解性は予測不能になる。問題の深刻度は予測しない。係数は abstract にない | ★(abstract) |
| Lavazza, Morasca & Gatto, EMSE 2023, DOI 10.1007/s10664-023-10396-7 | 学生による保守タスクの完了時間 | 結果は取得できなかった | 無印 |

Muñoz Barón らの研究で、SonarQube 既定の閾値 15 を超えるスニペットを含んでいたのは 2 研究だけだった。このため著者らは閾値を推奨できないとし、「できるだけ低く」とだけ述べている。元の研究はいずれも cognitive complexity を測るために設計されたものではなく、Kendall τ は r に換算されている。どの研究も Java 中心のスニペット規模の実験室課題で、Rust には移せない可能性がある。rust-code-analysis は CoC を独自に実装しており、それが Rust の理解性に照らして妥当かを確かめた研究はない(調査者の推論)。2024〜2026 年の cognitive complexity の追試は見つからなかった。GazeCopilot(arXiv 2511.08177)は CoC を難度合わせに使っただけで、妥当性は検証していない。

### 関数単位では CC と LOC は冗長でないが、集約すると size に支配される

Landman, Serebrenik, Bouwers & Vinju(J. Softw. Evol. Process 2015/2016, doi:10.1002/smr.1760)は、Java の 1,760 万メソッドと C の 630 万関数を調べた。SLOC と CC の線形相関は、サイズとともに分散が大きくなるため「中程度にとどまる」。文献にある R² = 0.60〜0.95 は、集約や変換をした結果である。調査者が読んだのはブログ要約だけで、論文本体は ★ ではない。また結論はいずれも Java/C に基づく。CC が欠陥予測で size を上回る予測力を持つかを 2022〜2026 年に単独で検証した研究は見つからなかった。maintainability index は Halstead・CC・LOC の固定式なので size の交絡を受け継ぐ。2022〜2026 年に MI を結果変数と照らした検証も見つからなかった。Ardito et al.(PeerJ CS 7:e406, 2021)は rust-code-analysis で 9 つのアルゴリズム実装を Rust・C・C++・Python・JS・TS にわたって比較した。Rust は C/C++ より CC・Halstead・MI で良く、cognitive complexity は全言語中最低だった。ただし結果変数のない記述的比較で、妥当性については何も言えない(無印)。

### スメルの害は一貫しているが小さく、検出器どうしは一致しない

Palomba et al.(EMSE 2018)は Java の 30 プロジェクト・395 リリースで、手作業で検証した 17,350 件を調べ、スメルのあるクラスは変更・欠陥を起こしやすいと報告した(無印)。Hall et al.(TOSEM 2014)は、有意な場合でも欠陥への効果は 10% 未満と小さく、闇雲なリファクタリングが欠陥を減らす見込みは薄いとした(無印)。Brown & Greer(ENASE 2023, 複数言語)は相関を「弱い」としている(無印)。Amit, Ben Ezra & Feitelson(arXiv 2103.01861, Java 31,687 ファイル)では、欠陥に対して潜在的に因果的と言えるスメルは 20% 未満だった(無印)。

検出器の一致については、Paiva et al.(JSERD 2017, Java)が inFusion・JDeodorant・PMD・JSpIRIT を比較した。全体一致率は 83〜98% だが、これはスメルのない実体で水増しされている。スメルのある実体だけで見ると、ペアごとの一致は 0.38%〜64.56% だった(無印)。Ichtsis et al.(TechDebt 2022)も、単純なスメルですら一致が低いと報告している。arXiv:2110.02916 が引く研究では、PMD の Large Class 検出は precision 100%・recall 14% だった(無印)。スメルの検出結果は、それぞれの検出器が採用した操作的定義の産物と見なすべきである、というのが調査者の推論である。God Class や Feature Envy のようなクラス単位のスメルは、Rust の struct・trait・module にうまく対応しない。

### 閾値はベンチマークに相対的で、言語ごとに違う

Alves, Ypma & Visser(ICSM 2010)の方法は、OO の 100 システムからなるベンチマークの分布から閾値を導く。得られる閾値はベンチマーク相対であって、結果変数で検証されたものではない(無印)。ComSIS の論文(DOI 10.2298/CSIS181012035B, 2019 年頃)は 9 メトリクスを Java・C++・C#・Python で比べ、閾値が言語間で異なることを示した(無印)。Rust の閾値セットやベンチマークコーパスは存在しない。単一リポジトリで同じ考え方を適用するなら、プロジェクト内パーセンタイルで閾値を取るのが最も近い、というのが調査者の推論である。

### 計算された理解性スコアは実際の理解を捉えない

Scalabrino et al.(ASE 2017; TSE 2019)は、Java で 121 メトリクス、63 名の開発者による 444 評価を調べた。readability や複雑度を含め、どのメトリクスも理解性を捉えられず、組み合わせたモデルも「実用には低すぎる」とした(無印)。Trockman et al.(MSR 2018)は同じデータを再分析し、組み合わせの有効性をより強く主張している(無印)。Wyrich et al.(ACM CSUR 2023, arXiv:2206.11102)は 1979〜2019 年の 95 実験を調べ、実験設計が非常に不均質であることを示した(無印)。したがって「理解性で検証済み」という主張は、理解の何を測ったか(時間・正しさ・評価)に依存する。Segedinac et al.(2024, Python, 学生 90 名)では、視線特徴だけで学習したモデル(F = 0.85)が、構造的なコード特徴を加えたモデルを上回った(無印)。

### 業界の「code health」:ベンダー自身の研究と生産性調査

CodeScene の研究(Tornhill & Borg, TechDebt 2022, arXiv:2203.04374, ★(abstract))は、39 のプロプライエタリコードベース・30,737 ファイルを調べた。低品質コードは欠陥が 15 倍、課題解決時間が平均 124% 増、最大サイクルタイムが 9 倍だった。abstract には言語が書かれていない。Borg, Pruvost, Mones & Tornhill(TechDebt 2024, arXiv:2401.13407, ★ HTML)は 79 プロジェクト・46,211 ファイルを調べた。主に C++ と C# で、84% 超のファイルが「healthy」である。欠陥と CH の関係は、CH 1→5 で減少し、5〜8 で横ばいになり、8 以上で再び減少する非単調な形だった。3 次多項式の当てはまりは R² = 0.038(欠陥)・0.006(時間)で、著者自身が予測ではなくパターンを示すものだと述べている。著者が明記する限界は、LoC と churn を統制していないこと、change coupling を無視していること、顧客だけを対象とした選択バイアス、関連の分析にとどまることである。履歴担当の調査者はこの曲線の形を二次要約(Moonlight)で得ていたが、メトリクス担当の調査者が HTML で同じ形を確認している。

「AI コーディング支援は不健康なコードで欠陥リスクを 30% 以上上げ、CH 7.0 未満で急増する」という主張は、CodeScene のプレスリリース(PR Newswire, 2026-01-28)に出てくるものである。論文 "Code for Machines, Not Just Humans" はどちらの調査者も読んでおらず、ベンダーの自己報告として扱う。

Google の Cheng et al.(ESEC/FSE 2022 industry)は、ラグ付きパネル分析で、知覚されたコード品質の改善が知覚された生産性の改善に先行することを示した(無印)。両変数とも調査票によるもので、静的メトリクスは一切関与しない。SonarQube の "Clean Code" 分類を結果変数で検証した研究は見つからなかった。

## クローン検出

### Type-1/2 は成熟した。Type-3/4 は道具ごとに結論が割れる

Type-1/2 の検出は「ほぼ完成に達した」とされ、研究の重心は Type-3/4 に移っている(Pizzolotto, Matsushita & Inoue, IPSJ SIG-SE 2022-SE-211, 査読なし, ★)。SourcererCC(ICSE 2016, arXiv:1512.06448, ★(abstract), Java/C/C#)は bag-of-tokens による索引で、250 MLOC を 1 台のワークステーションで扱える。

jscpd v5 は Rust 製の自己完結バイナリで、ローリング Rabin–Karp ハッシュで重複トークン列を探す。README は 224 形式をその言語の定義どおりにトークン化するとしている。既定の出力は完全一致だけで、opt-in として次のオプションがある(★ README)。

- `--ignore-identifiers` / `--ignore-literals`(Type-2)
- `--max-gap-lines` / `--similarity`(Type-3、tree-sitter ベース、Rust を含む)
- `--semantic`(experimental。重複だけでなく、クライアント関数と対応するサーバのエンドポイントのような「関連コード」も報告する)

ベンチマークはプロジェクト自身の `fixtures/` だけで、独立した precision/recall 評価はどの言語についても見つからなかった。調査者が読んだ README には `weak`/`mild`/`strict` のモード名が出てこない。そのため 5.3.x の `weak` が v3/v4 と同じ意味かは確認できていない。監査ツールの README には「default weak mode では空白とコメントを無視する」とあるが、これはツール側の記述で、今回の調査では検証していない。

学習モデルの汎化は弱い。Pinku, Mondal & Roy(ICSME 2024, arXiv:2412.14739, ★(abstract))では、単一言語モデル(ASTNN、GMN、CodeBERT)は BigCloneBench で F1 が高いが、SemanticCloneBench では最大 20% 変動し、変異にも弱かった。LLM の結果は次のとおりである。

| 研究 | データ | 結果 | ★ |
|---|---|---|---|
| Khajezade et al., ICPC 2024 ERA, arXiv:2401.13802 | 言語横断・単一言語の Type-4 | GPT-3.5-turbo が F1 0.877(言語横断)、0.878(単一言語) | 無印 |
| Zhu, Yoshida, Choi, Matsubara & Takada, arXiv:2511.01176 (2025) | CodeNet・BigCloneBench 由来の 7 データセット、5 LLM × 4 プロンプト | o3-mini が CodeNet 系で F1 0.943、BigCloneBench 系で「大きく低下」。5 回の再投入で判定の 90% 超が不変、F1 の変動は 0.03 未満 | ★(abstract) |
| Dou et al., arXiv:2308.01191 (2023) | 複数言語 | 高度な LLM が複雑な意味的クローンで既存手法を上回り、CoT が効く。言語によって性能が変わる。数値は読めていない | ★(abstract) |

良い LLM 結果は、競技プログラミング系の小さく自己完結したスニペットで得られている。そこでの「クローン」は同じ問題を解くコードという意味で、アプリケーション内の Type-3 とは性質が違う(調査者の推論)。LLM のコストはペア数に比例するので、現実的には安い検出器が出した候補の選別にしか使えない(同推論)。Rust のクローン学習データは見つからなかった。

### ベンチマークの多くは測りたいものを測っていない

Krinke & Ragkhitwetsagul は 2022 年(IWSC, 無印)に WT3/T4 の 86% が偽陽性だと報告していた。2025 年の続報(arXiv:2505.04311, ★ HTML)は、43 機能にわたって層化抽出した 406 ペアのうち 379 ペア(93.3%)が機能を共有しないことを示した。評定者間の一致は 89.7%、κ = 0.476 である。報告上 P 99.8 / R 88.3 / F1 93.7 の論文は、補正後の F1 が約 6.3% になる。また WT3/T4 の 8,498,894 ペアのうち 4,651,096 ペアは "Copy File" という一つの機能に属し、その機能だけを学習したモデルでも約 55% の recall が出る。BigCloneBench は構文的 Type-1/2/3 検出器の recall 評価にはなお使えるが、precision の測定には使えない。

他のベンチマークについて、同じ論文は次のように評価している。

- POJ-104(C/C++, 52,000 件)は判定機で検証されており、内在的な欠陥はない。ただし CodeXGLUE のクローン課題は改変した BigCloneBench を含むので、その問題を受け継ぐ。
- GPTCloneBench(arXiv:2308.13963)と SemanticCloneBench(4,000 ペア、Java・C・C#・Python)には欠陥の報告がない。ただし BigCloneBench ほど精査されていない。

precision の測定は本質的に主観的で、測る人によって大きく変わる(Svajlenko & Roy survey, arXiv:2006.15682, ★ 該当節)。設定の影響も大きい。Wang, Harman, Jia & Krinke(FSE 2013, Bellon suite)は、使われている設定より有意に良い設定が存在することを示した(無印)。Ragkhitwetsagul et al.(SSBSE 2016, Mockito)では、最適化したパラメータはクローン一致を 19.91〜66.43% 改善したが、クローンの質は悪化した(無印)。Rust のベンチマークは存在しない。人間が対処する報告の precision を測る公開ベンチマークはないので、ツール自身の報告を標本にとって refactor/preserve でラベル付けするしかない、というのが調査者の推論である。

### どのクローンを統合する価値があるかの証拠は古く、結論も割れている

| 研究 | データ・言語 | 結果 | ★ |
|---|---|---|---|
| Juergens, Deissenboeck, Hummel & Wagner, ICSE 2009, arXiv:1701.05472 | 5 システム(Munich Re の C# 3 件、COBOL 1 件、OSS Java の Sysiphus)。CloneDetective、10 正規化文以上、メソッド境界内 | クローン群 1,427 のうち 724(52%)が不整合、そのうち 203(28%)が意図しない不整合。確認された欠陥 107 件で、意図しない不整合の平均約 50%(システムごとに 33〜64%)が欠陥。保守工数への影響は未決 | ★ |
| Wagner, Abdulkhaleq, Kaya & Paar, arXiv:1611.08005(EASE 2016 と記憶されているが未確認) | 産業 Java 3 システム(253〜454 kLOC)。ConQAT、最小長 20/10 | Type-3 群の 17% に欠陥(3〜33%)。長さと欠陥は ρ = 0.268, p = 0.12, N = 35 で有意でない。Type-3 は「他のコードより欠陥を起こしやすいとは見えない」。システム間の差は開発者の認識で説明される | ★ |
| Mondal, Roy & Schneider(IWSC 2014; SQJ 2016; JSS 2018) | 6〜8 システム、2〜3 言語 | 遅延伝播は Type-3 とブロッククローンで強い。Type-1〜3 のうち Type-3 が最もバグを起こしやすい | 無印 |
| Göde et al.(二次要約) | — | クローンの 88% は一度も変更されないか 1 回だけ。意図しない不整合はクローン変更の 15% | 無印 |

Type-3 の害については、Mondal ら(バグを起こしやすい)と Wagner ら(産業 Java で差なし)が対立している。前者は複数システム・複数言語で改訂履歴を長く追った点が強みである。後者は産業システムで、欠陥を文書で確認した点と、開発者の認識という説明変数を示した点が強みである。どちらも Rust ではない。

共変更に基づく候補の順位付け(Mondal et al. CSMR-WCRE 2014/ICSME 2014)や、34 特徴で学習した推薦器 CRec(arXiv:1807.11184)もあるが、精度は読めていない(無印)。PLOS One 2022(PMC9714808)の 5 システムの縦断研究では、実際にリファクタリングされたクローンはわずかだった(無印)。2022〜2026 年に Juergens 型の欠陥研究を追試したものは見つからず、Rust でも見つからなかった。

### Rust 固有の歪み:トレイト実装の定型、マクロ、テスト

以下はすべて Pizzolotto et al. 2022(★)による。Type-1/2 として分類した 3,033 件(15 crate、CCFinderSW、t = 65 tokens)のうち、`mut` やライフタイムといった Rust の制約が原因のものはなかった。

- 最多の類型は、共通メソッドやトレイト(`len`、`fmt`/Debug など)の手書き実装を型ごとにコピーしたもので、全プロジェクトに少なくとも 1 件あった。著者らは「マクロで容易に置き換えられる」としている。
- 次に多いのは、パラメータ数だけが違うメソッドのコピーで、半数以上のプロジェクトに見られた。
- インラインの `#[test]` やベンチマークのコードも多い。crossbeam-channel の 807 件はテストを除くと 351 件になった。
- crate 間の差は非常に大きい(slab 11、cgmath 328、bitvec 561、crossbeam-channel 807)。
- HIR で検出すると、脱糖のせいで短いソースがトークン閾値を超える(`?` が完全な match に展開されるなど)。マクロ依存の crate では件数が爆発した(cgmath 328 → 165,834、ndarray 396 → 30,669)。著者らは、HIR はソースレベル検出の補助にとどめるべきだとしている。
- `#[derive]` で生成された実装は `#[automatically_derived]` で除外し、`vec!`/`write!`/`println!` は正規表現で除外したが、crate 独自の `macro_rules!` は除外していない。

2022 年時点で CCFinderX と NiCad は Rust 非対応だった。MSCCD(arXiv:2204.01028)は Rust を組み込みで支援する。CCFinderSW は generic-array と petgraph で 24 時間以上かけても報告を出せなかった。査読付きの Rust クローン研究と、Rust の Type-3 研究は見つからなかった。調査者の推論として、Type-2 の正規化実行は定型による誤警報が集中する場所である。そのため、全体の閾値を上げるより、`#[cfg(test)]` や既知のトレイト形の `impl` を別枠に分けるほうが証拠と整合する。ただしこれは Rust で測定されていない。

## 履歴・アーキテクチャ解析

### ホットスポットの考え方そのものより、独立したプロセスメトリクス研究のほうが強い支持を持つ

ホットスポット(変更頻度 × 複雑度)を直接検証した研究は CodeScene の著者によるものしか見つからなかった(前節)。CodeScene の Code Health は独自のスメル集合を含む合成指標である。したがって "Code Red" が検証しているのは CodeScene の指標と活動量の組み合わせであって、「RCA 複雑度 × コミット数」ではない(調査者の推論)。間接的な支持として、Mo, Cai, Kazman et al.(TSE 2021, 19 プロジェクト, 無印)は、エラーや変更を起こしやすいファイルが「単独で存在することはまれ」で、アーキテクチャ上つながっていると報告している。

欠陥予測については次のとおりである。

- Majumder et al. 2022 の数値は主要な知見のとおりである。著者らは、小規模研究から得たメトリクス重要度は規模を大きくすると「劇的に変わり得る」とも警告している。
- Rahman & Devanbu(ICSE 2013, 12 プロジェクト・85 リリース, 無印)は、静的メトリクスはリリース間でほとんど変わらないため、モデルが毎回同じファイルを指す(stasis)と論じた。
- Kamei et al.(TSE 2013)の 14 の変更レベル特徴量と、McIntosh & Kamei(TSE 2018)の「モデル性能と特徴量重要度は時間とともにドリフトする」という結果は [prior knowledge] である。
- JIT 予測では、単純な基準を超えるのが難しい。Zeng et al.(ISSTA 2021, 31 万件超の変更, 無印)では、追加行数だけのロジスティック回帰(LApredict)が CC2Vec と DeepJIT を上回った。ReDef(arXiv:2509.09192, C/C++ 22 プロジェクト, ★(abstract))では、追加ブロックと削除ブロックを入れ替えても CodeBERT などの性能はほとんど変わらず、モデルは表面的な手がかりに依存していると結論している。
- 欠陥予測の順位がリファクタリング候補の順位として良いかを直接評価した研究は見つからなかった。

### 共変更結合はパラメータに強く依存する

MDPI *Software* 2024(DOI 10.3390/software5010011, 5 リポジトリ, 低位の venue, 無印)では、co-change strength の閾値を上げるとリンクが急減し、40% 以上ではファイルあたりリンク数の最頻値が 0 になった。Bochicchio, Sas, Gilardi & Arcelli Fontana(IST 2025 / SANER 2026 journal-first, Java, Arcan 2, 無印)では、アーキテクチャスメルを含むペアのほうが、共変更率の中央値が高かった。

Herzig & Zeller(MSR 2013)の tangling の結果は [prior knowledge] である。実務ツールの issue にはいくつかの経験則が見られるが、これらは研究ではない。

- 99 パーセンタイル超のコミットを除外する(下限 500 行、gitmole #72)
- 1 コミット 30 ファイルで打ち切り、マージは飛ばす(repo-understanding #6)
- PR を 1 changeset として扱うとペア数が変わる(Ctxo #165)

コミット単位と PR 単位のどちらが正しいかは検証されていない。rename 追跡がないと、ファイルを移動するたびに履歴が切れる。調査者の推論では、これはまさにリファクタリングツールが誘発するモジュール分割で最も効く。最大 changeset サイズ、squash merge、rename 追跡の効果を効果量つきで分離した 2020〜2026 年の研究は見つからなかった。Kovalenko et al.(ASE 2018)は取得できなかった。

### アーキテクチャスメル研究は構造と履歴を組み合わせるが、対象は Java と C/C++ だけである

Mo, Cai, Kazman, Xiao & Feng(TSE 47(5), 2021)は 6 つのアンチパターンを定義した:Unstable Interface、Modularity Violation、Crossing、Unhealthy Inheritance、Clique、Package Cycle。これらは構造と改訂履歴から検出され、19 プロジェクトでバグ・変更の起こしやすさと関連していた。寄与が「群を抜いて大きい」のは Unstable Interface と Crossing である(無印、一覧は二次要約による)。

Sas, Avgeriou, Pigazzini & Arcelli Fontana(JSEP 2022, Java 31 システム, Arcan, 無印)では、分析した 87% のコミットで、スメルを 1 つ以上持つ成果物のほうがよく変更されていた。ただし効果は中〜大規模の成果物で強く、size の代理になっている可能性がある(調査者の推論)。Decoupling Level(ICSE 2016)は [prior knowledge] である。

Arcan は Java・C・C++ だけに対応する(★ wiki)。Rust を解析できるアーキテクチャスメル検出器(Arcan、DV8、Designite)は見つからなかった。Rust では crate 間の循環は Cargo が禁じているので、循環スメルが意味を持つのはモジュール単位だけである。そのモジュール単位の循環が有害だという Rust での検証はない(調査者の推論)。

### エージェントのコミットと squash は、履歴メトリクスへの影響が未定量である

AIDev(Li, Zhang & Hassan, MSR 2026, arXiv:2602.09185, 無印)は 932,791 件のエージェント PR を集めている(Codex、Devin、Copilot、Cursor、Claude Code、116,211 リポジトリ)。ただしこれを使った研究は、マージ結果やレビューの力学を調べたもので、churn や結合への歪みは扱っていない。エージェントの作業様式が変更頻度や結合を膨らませるという指摘は調査者の推論で、文献上は未検証である。

### ツールの状況(★ GitHub API, 2026-10-24)

| リポジトリ | 状態 | 最終 push |
|---|---|---|
| regexident/cargo-modules | 活発 | 2026-10-21 |
| jplatte/cargo-depgraph | **archived** | 2026-05-13 |
| est31/cargo-udeps | 活発 | 2026-10-22 |
| bnjbvr/cargo-machete | 活発 | 2026-10-19 |
| git-truck/git-truck | 活発 | 2026-10-12 |
| adamtornhill/code-maat | 低活動、archived ではない | 2025-07-03 |
| smontanari/code-forensics | 停止 | 2023-01-06 |
| erikbern/git-of-theseus | 停止 | 2023-11-25 |

CodeScene は Rust を「full support」としている(Code Health、メソッド単位の X-Ray、埋め込みユニットテストを別扱いにするオプション)。ただしこれは商用製品のベンダー文書である(無印)。

## LLM 併用

### スメル検出は「決定的な検出器が候補を出し、LLM が判断を足す」形が最良だが、偽陽性も足す

- Souza et al.(arXiv 2601.09873, v2 2026-05, 体裁は TSE だが venue 記載なし, ★(abstract))は、Java 30 プロジェクトの 9 スメルを、開発者 76 名が 268 候補を検査した正解と照合した。
  - LLM は Large Class と Long Method で強かった。
  - LLM と静的ツールのハイブリッドは 9 スメル中 5 つで F1 が最良だったが、複雑なスメルでは偽陽性が増えた。
- arXiv 2504.16027(Java・Python・JS・C++)は、二次要約によれば precision が GPT-4.0 で 0.79、DeepSeek-V3 で 0.42 で、どちらも recall が弱い(無印)。
- iSMELL(ASE 2024, Java 3 スメル)は検出 F1 75.17% である(無印、abstract は切れていた)。
- 調査者の推論では、LLM が強い Long Method と Large Class はサイズ・複雑度の順位付けがすでに見つけるもので、ここで LLM を使っても既存の信号と重複する。

### LLM リファクタリングの正しさは外部の検証で回復されている

| 研究 | 区分 | 言語 | 結果 | ★ |
|---|---|---|---|---|
| Cordeiro et al., arXiv 2411.02320 (2024) | 独立 | Java 30 プロジェクト | StarCoder2 のテスト通過 pass@1 28.36%(開発者の採用済みリファクタリングは 100%)。通過したものに限ると、スメル削減率は開発者を 20.1% 上回る。one-shot は zero-shot よりテスト通過率が 6.15% 高い | 無印 |
| Pomian et al., EM-Assist, arXiv 2401.15298 / FSE 2024 demo | 区分未確認(ノートに著者所属の記載なし) | Java/Kotlin | 2,849 例で最大 62.3% が hallucination。IDE の検査で除外し、IDE のエンジンで適用する。1,752 例で開発者の実際の選択が top-5 に入る率 53.4%(従来最良 39.4%)。調査の数値は版によって異なる(81.3% / 94.4%) | 無印 |
| CodeScene "Refactoring vs Refuctoring" (2024-01) | **ベンダー**、この形では査読なし | 不明 | 生の LLM で正しいのは 37%(GPT-3.5 は 26〜33%)。約 10 万例で学習した事実確認モデルが残した候補の 98% が改善かつ振る舞い保存。独立追試なし | 無印 |
| Horikawa et al., arXiv 2511.04824 (2025) | 独立 | AIDev の Java | エージェントのコミットの 26.1% がリファクタリング目的。型変更や改名など小さく局所的なものが中心。構造メトリクスの改善は小さいが有意 | 無印 |
| Ottenhof et al., arXiv 2601.20160, MSR 2026 | 独立 | AIDev の Java、RefactoringMiner + DesigniteJava 3.0 | エージェントのリファクタリングは注釈の変更が中心。スメルが有意に増えたのは Cursor だけ。効果量は abstract にない | ★(abstract) |

調査者の推論として、Rust では `cargo check`、clippy、テストが自然な検証器になり、厳格なコンパイラが壊れたリファクタリングをより多く弾く可能性がある。しかし未検証であり、Rust や所有権意味論を持つ言語での LLM リファクタリング研究は見つからなかった。

### LLM 判定者は順序・名前・言語で揺れる

- CodeJudgeBench(arXiv 2507.10535, ACL 2026, 5,352 ペア, 26 judge, 無印):全モデルが、応答の順序、変数名、誤導的コメントに対して有意に不安定だった。
- arXiv 2406.07791(12 judge, 無印):位置バイアスは、候補どうしの品質差が小さいときに強く出る。
- CoReEval(arXiv 2510.16579, ICSE 2026 poster, 140 万件超, 無印):readability の判定と人手評価の相関は、言語によって 0.25 から 0.00 まで下がる。開発者向けに工夫したプロンプトは観点の網羅を良くするが、スコアの分散を増やす。
- Wang et al.(arXiv 2502.06193, ISSTA 2025, 無印):コード翻訳では Pearson 約 0.81 まで上がるが、冗長さへのバイアスがある。
- 機械生成のレビュー要約(二次・未検証)によれば、LLM のクロスエントロピーで保守性を予測できるのは LOC を統制したときだけで、LOC のほうが強い予測子だった。
- 見つからなかったもの:コード品質判定の κ、自己選好の測定、rubric 方式と pairwise 方式の直接比較。

### 産業の AI レビュー:受容率は 5〜74% で、分母は比較できない

| 報告 | 区分 | 規模 | 主な数値 | ★ |
|---|---|---|---|---|
| Google blog 2023-05 / ICSE-SEIP 2024 | **ベンダー(自社)** | 社内 | 目標 precision 50%。プレビューした提案の 40〜50% が適用された。ML 編集で対処されたレビューコメントは 7.5%(スニペット)だが、ICSE abstract では約 5% で、両者は一致しない | blog ★ |
| Cihan et al., ICSE-SEIP 2025, arXiv 2412.18531(Qodo PR-Agent 導入の研究) | 学術・産業の共同研究(ツール提供者ではない) | 約 238 名・10 プロジェクト | AI コメントの 73.8% が resolved。PR クローズ時間は 5h52m から 8h20m に**増加** | 無印 |
| Atlassian RovoDev, arXiv 2601.01129, ICSE-SEIP 2026 | **ベンダー** | 1,900 超のリポジトリ、5.4 万コメント | resolved は AI 38.70%、人間 44.45%。PR サイクル中央値は 30.8% **短縮** | 無印 |
| ByteDance BitsAI-CR, arXiv 2501.15134 | **ベンダー** | 週 1.2 万人超 | precision 75.0%。生成のあとに検証段がある 2 段構成 | 無印 |

サイクルタイムへの効果は報告によって向きが逆である。Qodo の研究はツール提供者以外が行った点が強みで、Atlassian の報告は規模が強みである。防いだ欠陥数を測った研究は一つもなく、どれも「resolved」や「accepted」という代理指標である。

### AI 生成コードの品質:重複の増加が最もよく繰り返し観測されている

- GitClear 2025(**ベンダー**、2020〜2024 年の 2.11 億変更行、無印):隣接コードと重複する 5 行以上のブロックが 2024 年に 8 倍になった。moved 行は 39.9% 減った(変更に占める割合は 24.1% から 9.5%)。相関であって因果ではない。
- METR RCT(2025-07、独立、16 名・246 タスク、無印):AI を使うとタスクが 19% 長くかかったが、本人たちは事後に 20% 速くなったと信じていた。
- Uplevel(**ベンダー**、約 800 名):バグが 41% 増えた。チーム間の比較である。
- CodeRabbit(**ベンダー**、470 PR):AI の PR は 1.7 倍の issue を含んでいた。ただし issue を数えたのは同社のツール自身で、循環のリスクがある。
- Fu et al.(arXiv 2310.02059, TOSEM, 733 スニペット、無印):弱点を含んでいたのは Python の 29.5%、JavaScript の 24.2% だった。
- arXiv 2607.21832(★(abstract)):数値は abstract にない。
- arXiv 2605.22534(無印):Claude Code の PR は 213 件しかなく、エージェント別の比較は Claude Code について支持が薄い。
- 過剰な抽象化、デッドコード、規約の逸脱を厳密に調べた研究は見つからなかった。

### 決定的な解析に LLM の選別を重ねる手法と再現性

- Du et al.(Tencent, ICSE-SEIP 2026, arXiv 2601.18844, 433 警告、無印):LLM と静的解析を組み合わせると、偽陽性の 94〜98% が除去され、「高い recall」を保った。ただし真陽性は 105 件しかない。
- arXiv 2506.16899(OWASP Benchmark、無印):真陽性を一つも失わないという制約の下で、単一の LLM は偽陽性の約 62.5% を除去し、複数の LLM を組み合わせると約 78.9% になった。
- LLM4PFA(arXiv 2506.10322、無印):誤警報の 72〜96% を除去し、recall は 0.93(45 件中 42 件)だった。
- arXiv 2607.09979(CERT/CMU 系とされる、日付・venue 未確認):recall 98% 以上、specificity 94.8% 以上で、繰り返し実行の一貫性検査を使う。
- Ouyang, Zhang, Harman & Wang(TOSEM, arXiv 2308.02828、無印):既定設定では、繰り返し要求してテスト出力が一度も一致しないタスクの割合が 75.76%(CodeContests)、51.00%(APPS)、47.56%(HumanEval) だった(v2 の数値。v1 では 72.73%、60.40%、65.85%)。temperature 0 でも非決定性はなくならない。

コード品質やスメルの判定、クローン報告やホットスポットの LLM 選別について、安定性を測った数値は見つからなかった。

## Rust ツール群と振る舞い保存

### 固定版と最新版の差(★ gh api / crates.io, 2026-10-24)

| ツール | 監査ツールの固定版 | 最新 | 状態 |
|---|---|---|---|
| rust-code-analysis | 0.0.25 | 0.0.25(2023-01-13)。crates.io の `rust-code-analysis-cli` も 0.0.25 が最大 | archived ではない。master の最終コミットは 2026-01-20。`pushed_at` は 2026-04-06 だが、他ブランチへの push でも更新されるので、生存の指標としては弱い |
| jscpd | 5.3.1 | v5.4.2(2026-10-20) | 活発 |
| ast-grep | 0.44.1 | 0.51.0(2026-10-15) | 活発。固定版は minor で 7 つ遅れている |
| cargo-modules | 0.26.0 | 0.28.0(crates.io 2026-10-18) | 活発 |
| Code Maat | 1.0.4 | v1.0.4(2023-02-20)が最新 | 最終 push は 2025-07-03 |

rust-code-analysis(RCA)の詳細は次のとおりである(★)。2026-01-20 の master コミットには次のものが含まれる。

- tree-sitter 0.26.3 への更新と、同じ日の revert
- "Expose inner tree-sitter::Node"(#1210)
- toml の依存更新

open な issue のうち重要なものは 2 つである。

- **#1140**(2025-03-23 起票):`cargo install rust-code-analysis-cli` が `expected tree_sitter::Language, found a different tree_sitter::Language` で失敗する。回避策は `--locked` で、2026-01-18 のコメントはライブラリとしての利用が依然壊れていると報告している。
- **#1249**(2026-10-06 起票、コメントなし):0.0.25 と master 37e5d83c を、McCabe と SonarSource Cognitive Complexity whitepaper v1.7 に照らして調べたもの。起票者は、報告は AI 支援で書いたが、再現と数値は自分で確認したと述べている。挙げられている逸脱は次の 4 つである。
  - `match`:循環的複雑度が最後の arm も含めて全 arm を数える(4 arm で 5、期待値は 4)。
  - 裸の `loop`:循環的複雑度では判断として数える(3、期待値は 2)。cognitive では数えない(1、期待値は 3 = loop +1 と入れ子の `if` +2)。
  - `let … else`:判断として数えない(循環的 1、期待値は 2)。
  - `&&` と `||` の混在:cognitive が 5 になる(whitepaper の例では 4)。

ほかに、#341(クロージャの exit point)と #364(パーサごとのメトリクス差)が 2020 年から open のままである。Rust のクロージャや async の関数境界の誤りを報告した issue は見つからなかった。ただしこれは報告がないというだけで、正しいことの証拠ではない。

代替のうち、仕様に沿って Rust の関数単位 cognitive complexity を計算する保守中のツールとして見つかったのは SonarQube だけである(★ docs 2026.3:Cognitive/Cyclomatic Complexity、LCOV/Cobertura の取り込み、Clippy JSON を外部ルールとして取り込み、RSPEC-3776)。「Clippy ルール 122 本」はスニペット由来で、既定の閾値は未確認である。SonarQube が #1249 の事例で whitepaper どおりに数えるかの情報はない。コミュニティ製の Sonar Rust プラグイン 2 つは 404 だった(★)。

ほかのツールの位置づけは次のとおりである。

- tokei と scc はファイル・言語単位の計数器である(粒度は調査者の prior knowledge)。
- Lizard は活発(1.24.2、2026-10-22)だが、Rust 対応かは未確認である。
- cargo-geiger(geiger-0.14.0、2026-10-12)は unsafe の量を測る指標で、保守性の指標ではない。

### Clippy の size・複雑度 lint の位置づけ

`declare_clippy_lint!` の記述(rust-clippy master、2026-10-24)と、`cargo clippy --explain`(clippy 0.1.99、2026-10-13)で確認した(★)。

| lint | 群 | 既定の閾値・注記 |
|---|---|---|
| `cognitive_complexity` | restriction | `cognitive-complexity-threshold` 既定 25 |
| `too_many_lines` | pedantic | `too-many-lines-threshold` 100 |
| `too_many_arguments` | complexity | `too-many-arguments-threshold` 7 |
| `type_complexity` | complexity | — |
| `excessive_nesting` | complexity | `excessive-nesting-threshold` を clippy.toml で設定したときだけ発火する |
| `large_enum_variant` | perf | ジェネリック引数をゼロサイズとして扱う誤検知があると文書化されている。"Always measure the change this lint suggests" |
| `fn_params_excessive_bools` / `struct_excessive_bools` | pedantic | — |

`cognitive_complexity` の説明は "We used to think it measured how hard a method is to understand." で始まる。Known problems の節には、この lint は "has been left in `restriction` so as to not mislead users into using this lint as a measurement tool" とある。rust-clippy#3793 を参照し、代わりに `excessive_nesting` と `too_many_lines` を勧めている。

Li et al.(arXiv 2310.11738, ICSE 2024 poster, ★(abstract))によれば、crates.io の警告密度は平均 21/KLOC で、Rosetta では著者らの自動修正で 195 から 18/KLOC に下がった。調査で開発者が不満として挙げたのは誤検知と自動修正の欠如である(この部分は無印)。

### Rust 固有の品質研究は unsafe 中心で、スメル目録は存在しない

Evans, Campbell & Soffa(ICSE 2020)の「crate の約 29% が unsafe を直接使う」という数値は二次資料経由で、無印である。Astrauskas et al.(OOPSLA 2020)と Rudra(SOSP 2021)の数値も一次資料で確認していない。検証された Rust のコードスメル目録や、`unwrap`・`clone`・ライフタイムの数を欠陥や工数と照らした検証は見つからなかった。スメル検出研究の 77.1% は Java を対象としている(SLR arXiv 2012.08842、無印)。

### テストの感度:cargo-mutants と、ミューテーションスコアの妥当性をめぐる対立

cargo-mutants の現行版は v27.1.0(2026-06-02、最終 push 2026-10-19)である。mutants.rs のブックから確認した機能と注意点は次のとおりである(★)。

- `--in-diff DIFF_FILE` は、変更領域に重なるミュータントだけを試す。
  - 他の領域のテストが不足するおそれがあると、ブック自身が注意している。
  - テストコードだけを変えた diff では、"may have a very material effect on test coverage" であるにもかかわらず、ミュータントが一つも走らない。
- `--shard k/n` は実行時の協調なしに分割する。全シャードに同一の引数を与える必要がある。
- `--test-tool=nextest` に対応する。ただし、最初の失敗のあとに遅い結合テストが残って走り続けるため、nextest のほうが遅くなる場合がある(nextest discussion #2482)。
- 既定のタイムアウトはベースラインのテスト時間の 5 倍で、最小 20 秒である。
- `#[cfg(test)]`、テスト属性の付いた関数、`#[mutants::skip]`、`unsafe` fn は変異の対象外である。
- 中心的な変異の種類は `FnValue` で、関数本体を型に合った推測値に置き換える。

周辺ツールの版は、cargo-llvm-cov v0.9.1、tarpaulin 0.37.5、cargo-nextest 0.9.149 である(★)。

ミューテーションスコアの妥当性については、研究どうしが対立している。

- Just et al.(FSE 2014, Defects4J Java, 無印)は、ミューテーションスコアが実際の欠陥の検出と有意に相関するとした。
- Papadakis et al.(ICSE 2018, CoreBench + Defects4J, 無印)は、テストスイートのサイズを統制すると相関はすべて弱くなるとした。ただし、同じサイズの無作為なスイートより、スコアで選んだスイートのほうが欠陥を有意に多く検出した。
- Zhao, Zhou & Cohen(ISSTA 2026, arXiv 2607.22880, ★(abstract))は LLM が生成したスイートで追試し、サイズは支配的な交絡要因ではないとした。カバレッジとミューテーションは、テスト対象のコードにバグがないと仮定できる回帰型の状況では有用だが、既存のバグを暴くのには信頼できない。係数は abstract に載っていない。

Papadakis らは人手で書かれたスイートで欠陥コーパスを使った点が強みで、Zhao らは最新かつ追試として設計された点が強みである。どちらも Java 系で、Rust の研究はない。

### リファクタリング検出と振る舞い保存の検査

- RefactoringMiner は 3.1.6(2026-09-16)で、対応言語は Java、Python(3.14 を含む)、Kotlin(precision/recall の検証項目は未チェック)、C/C++(CDT)である。Rust は対応表にない(★)。
- RefDiff は 2021-04-04 以降 push がない(★)。
- RefactoringMiner++(arXiv 2502.17716)と ActRef(arXiv 2505.06553, Python)は、振る舞いを変える編集とリファクタリングを分けることがなお難しいと述べている(無印)。

REM 2.0(Britton, Pak & Potanin, arXiv 2601.19207, 2026-01-27, ★(abstract))は rust-analyzer の extract-function の上に作られている。

- 常駐デーモンとして動き、借用検査のエラーをライフタイムやシグネチャの修正で直す「repairer」を持つ。裏返すと、借用の多い場面では素の rust-analyzer の抽出がコンパイルできないコードを出し得る。
- opt-in で CHARON → AENEAS → Coq の経路を使い、元のコードとリファクタリング後のコードの等価性を証明する。
- 20 リポジトリから取った 40 の抽出事例(async/await、const fn、非局所的な制御フロー、ジェネリクス、HRTB)の「大半」を扱える。
- 20 の検証ベンチマークについて、「現在の部分集合の範囲で」端から端までの証明を出す。成功件数は abstract にない。
- テストスイートは "necessarily incomplete" だと明言している。

検証器とテストツールは活発に保守されている(★):Kani 0.68.0、Creusot v0.14.0、Verus(2026-10-19)、Prusti(`prusti/prusti`、crates.io `prusti-contracts` 0.3.1)、proptest 1.12.0(crates.io。GitHub の latest release 0.9.6 は古いメタデータ)、quickcheck 1.1.0、bolero v0.13.7、insta 1.49.0。SWE-Refactor(arXiv 2602.03712)は、Java の振る舞い保存リファクタリング 1,099 件からなるベンチマークである(無印)。調査者は、毎コミット実行できるものを安い順に次のように並べた。

1. 既存のテストゲート
2. `cargo mutants --in-diff`
3. insta などによる golden master
4. proptest や bolero による新旧の差分テスト
5. 小さく純粋な関数に対する Kani の有界等価性ハーネス

完全な証明は GUI や async の多いコードでは毎コミットには非現実的だ、という判断は、REM の部分集合という但し書きと調査者の判断に基づく推論である。

## 現行の監査ツールの各部品に対して証拠が言えること

この節は、集めた証拠と各部品との対応を観察として並べたもので、決定でも推奨でもない。「推論」と書いた箇所は、資料の主張ではなく調査者または執筆者の推論である。

### RCA の関数メトリクス(LOC・循環的・cognitive)

- **支持する証拠**:関数単位では CC と SLOC は冗長でない(Landman et al., Java/C, 無印)。したがって両方を並べることは一応支持される。cognitive complexity は理解時間と r = 0.54 で相関する(Muñoz Barón 2020, 主に Java, ★)。README の方針「領域内で比較する」は、閾値が言語やベンチマークに相対的だという証拠(Alves 2010、ComSIS 2019)と整合する。閾値ではなく順位で見る使い方は、閾値に妥当性がないという結果(Muñoz Barón 2020)と矛盾しない。
- **反する証拠**:cognitive complexity が従来メトリクスを上回るという前提は、Lavazza 2023(無印)が否定している。Clippy 自身が `cognitive_complexity` を測定に使わないよう注記している(★)。RCA 0.0.25 の Rust での数え方は、#1249 の 4 つの構文で McCabe とも SonarSource 仕様とも食い違う(★ issue。起票者は AI 支援だが数値は自分で確認したと述べている)。リポジトリは実質休止で、修正が上流に入る見込みは薄い。理解性スコア一般が実際の理解を捉えないという結果もある(Scalabrino, Java, 無印)。
- **証拠が何も言わないこと**:Rust で、これらのメトリクスと理解・変更・欠陥との関係を調べた研究はない。ライフタイム、トレイト境界、マクロ、async、unsafe に由来する複雑さは、どのメトリクスにも反映されていない(推論)。tree-sitter の境界照合が検出できるのは関数境界の誤りで、#1249 のような数え方の偏りは検出できない(推論)。
- **前後比較への含意**(推論):`match` を `if` に、`loop` を `while` に書き換えるリファクタリングや、`let-else` を導入するリファクタリングでは、#1249 の偏りが前後で相殺されない。

### jscpd のクローン(識別子保持・正規化の 2 回、`--min-tokens 70`)

- **支持する証拠**:Type-1/2 には成熟したトークン方式で十分だという合意がある(Pizzolotto 2022, ★)。2 回の実行は、文献の Type-1 と Type-2 の区分に対応する。重複は AI 支援コードで最も繰り返し観測されている特徴である(GitClear, ベンダー。エージェントのリファクタリングが局所的だという独立研究とも整合)。2026 年時点でも、意味的クローン検出に、ベンチマークに依存しない妥当な代替は存在しない(arXiv:2505.04311, ★)。70 トークンは、文献で使われる値(Pizzolotto の 65 トークン、Juergens の 10 文、Wagner の 20/10 行)の近くにある。ただしどの値も導出されたものではない。テストを既定で除外し、マクロを展開しない設計は、テストが件数を膨らませ、マクロ展開が無用な報告を爆発させるという Rust での観察(Pizzolotto 2022, ★)と整合する。
- **反する証拠**:クローンの長さやサイズは害の予測子として支持されない(Wagner 2016, ★)。単一スナップショットの検出では、害の予測子として支持のある共変更・遅延伝播・不整合の履歴が見えない(Juergens、Mondal)。正規化した実行では、Rust で最多の類型であるトレイト実装の定型が「真陽性だが無用」として大量に出る可能性がある(Pizzolotto 2022 の HIR での観察からの推論)。
- **証拠が何も言わないこと**:jscpd v5 の独立した precision/recall 評価はない。Rust トークナイザの品質(raw 文字列、ライフタイムと char の区別、入れ子のブロックコメント、マクロのトークン木)も評価されていない。5.3.x の `weak` モードの意味は未確認である(README にはツール側の記述があるが、固定版のバイナリがこのチェックアウトにないため、手元で確認できなかった)。5.4.2 との差分は調べていない。Rust の Type-3 クローンの研究もない。

### ast-grep の限定ルール

- **支持する証拠**:検出器どうしが一致しないので、「スメル」は操作的定義の産物である(Paiva 2017、Ichtsis 2022, Java, 無印)。この点は、確立された規約だけを、範囲を限ってヒントとして書く現行の方針と矛盾しない(推論)。ast-grep のような構造検索は、既知の定型を除外するルールを書くのに使えるが、クローン検出器ではない、と調査者は推論している(文書は未確認)。
- **反する証拠**:見つからなかった。
- **証拠が何も言わないこと**:ast-grep ルールや規約検査の効果を測った研究はない。エージェントのコードの規約逸脱を厳密に調べた研究もない(LLM 担当調査者)。0.44.1 と 0.51.0 の差分は調べていない。

### Code Maat の共変更結合

- **支持する証拠**:欠陥予測では、履歴信号がプロダクトメトリクスを大きく上回る(Majumder 2022, ★(abstract)。反対例 Stanić & Afzal 2017 あり)。クローンの害の予測子として最も支持があるのは共変更である(Mondal、Juergens)。共変更が設計上の欠陥を確定しないという README の注意書きは、関連が観察的で、Java 中心で、modularity violation の寄与は小さい(Mo 2021)という証拠と整合する。
- **反する証拠**:結合の閾値を上げるとリンクが急減する(MDPI 2024, 無印)ので、既定値の選択は中立ではない(推論)。rename を追跡しないと、リファクタリングで生じるファイル移動のたびに系譜が切れる(推論)。モデルや重要度は時間とともにドリフトする(McIntosh & Kamei [prior knowledge])ので、全履歴をまとめて使うと現状とずれる(推論)。欠陥予測の順位とリファクタリングの必要性は別の目的である(推論)。
- **証拠が何も言わないこと**:マージを除いたコミット単位と PR 単位のどちらが妥当か、squash の効果、エージェントが書いたコミットの効果、短い履歴での安定性は、いずれも定量化されていない。履歴メトリクスを Rust で検証した研究はない。CodeScene のホットスポットに独立した追試はない。

### cargo-modules の依存グラフ

- **支持する証拠**:構造と履歴を組み合わせたアーキテクチャのアンチパターン(Unstable Interface = 高い fan-in と高い共変更、Crossing)は、バグ・変更の起こしやすさへの寄与が最も大きい(Mo 2021, 19 プロジェクト, Java/C/C++, 無印)。監査ツールは、構造(cargo-modules)と履歴(Code Maat)という、DV8 が組み合わせる 2 つの入力をすでに持っている(調査者の推論)。グラフの循環が欠陥を確定しないという README の注意書きと矛盾する証拠はない。
- **反する証拠**:見つからなかった。cargo-depgraph は archived である(★)。
- **証拠が何も言わないこと**:Rust のモジュール循環が有害かは検証されていない。Rust を解析できる研究用のスメル検出器はない。crate 間の DAG は構成上非循環なので、crate 単位では循環スメルは起こり得ず、意味を持つのは fan-in/fan-out か、crate をまたぐ共変更である(推論)。0.26.0 と 0.28.0 の差分は調べていない。

### 前後比較(`compare` と対応表)

- **支持する証拠**:Rust で使えるリファクタリング検出器はない(RefactoringMiner 3.1.6 は Rust 非対応、RefDiff は停止、★)。したがって、明示的な旧→新の対応表に既製の代替はない(調査者の推論)。移動や分割を推測しない方針は、絡み合ったコミットでリファクタリングとそれ以外を分けるのが難しいという研究(RefactoringMiner++、ActRef)と整合する。「メトリクスだけでは改善を証明しない」という README の記述は、静的メトリクスの妥当性が弱いという結果全般と、LLM リファクタリングで「メトリクスは改善したが振る舞いが壊れた」という結果(Cordeiro 2024 の 28.36%)と整合する。同じ版のツールで前後を比べる方式は、固定された偏りを部分的に相殺する(推論。ただし RCA の節の例外がある)。
- **反する証拠**:見つからなかった。ただし RCA の偏りが相殺されない構文変換がある(前述)。
- **証拠が何も言わないこと**:メトリクスの差分が理解のしやすさの改善と対応するかは、どの言語でも検証されていない。振る舞いの保存は比較の範囲外である。回帰型の状況でのテスト感度(Zhao 2026, ★(abstract))、`cargo mutants --in-diff`(★ ブック。テストだけの diff では何も走らないという限界つき)、REM 2.0(Rust の部分集合の extract function に限る)は、それぞれ別の軸の証拠である。

### refactor/preserve の判定台帳

- **支持する証拠**:クローンの欠陥率がシステム間で違う理由は、開発者がクローンを認識しているかで説明された(Wagner 2016, ★)。この結果は、人がレビューして判定を残す設計と整合する(調査者の推論)。人が対処する報告の precision を測る公開ベンチマークはないので、ラベル付きの判定を積み重ねたものが、このリポジトリで最も妥当なベンチマークになり得る(クローン担当調査者の推論)。LLM の出力は temperature 0 でも非決定的である(Ouyang, TOSEM, 無印)。だから、ソースの指紋を鍵にして判定を記録し、非決定的な判定を毎回出し直さない設計は、文献と整合する(LLM 担当調査者の推論)。産業の AI レビューは、提示する候補を減らして precision を上げる方向に調整されている(Google、ByteDance、いずれもベンダー)。
- **反する証拠**:見つからなかった。
- **証拠が何も言わないこと**:判定台帳そのものの効果を測った研究はない。preserve とした判定を記録して同じ報告を繰り返さないことがクローン追跡の提案と整合するという点も、測定はされていない(クローン担当調査者)。エージェントが記録した判定の安定性(同じ候補を繰り返し判定させたときの一致)の数値はない。文献で使われている管理策は、繰り返しサンプリング、順序の入れ替え、モデル ID と版の記録で、これらは判定結果のコード生成や SAST 選別での観察に基づく。

## 未検証・未調査

**領域をまたぐ欠落**

- 静的メトリクスやスメルについて、Rust で理解・変更・欠陥と照らした妥当性研究は一件も見つからなかった。`unwrap`・`clone`・unsafe・ライフタイムの数を検証した研究もなく、Rust のリファクタリング実践の研究もない。
- CodeScene のホットスポットや Code Health に、CodeScene 以外による追試がない。"Code for Machines, Not Just Humans" と「30%」の主張の一次資料は読めていない。
- squash merge とエージェントが書いたコミットが churn・結合・欠陥予測に与える影響は定量化されていない。最大 changeset サイズや rename 追跡の効果を効果量つきで分離した研究もない。
- コード品質やスメルについて LLM に繰り返し判定させたときの安定性の数値は見つからなかった。κ、自己選好、rubric 方式と pairwise 方式の比較もない。クローン報告やホットスポットの LLM 選別の研究もない。
- jscpd 5.3.x の `weak` モードの意味は確認できなかった。固定版のバイナリがこのチェックアウトに入っておらず、手元でも確認できなかった。jscpd v5 の独立した精度評価と、Rust トークナイザの品質も評価していない。
- 欠陥予測の順位がリファクタリング候補の順位として良いかを評価した研究はない。AI レビューが防いだ欠陥数を測った研究もない。

**静的メトリクス担当が再取得しなかった・取得できなかったもの**

- El Emam et al. 2001(TSE、クラスサイズの交絡):再取得しなかった。そこからの数値は引用しないこと。
- Sharma & Spinellis 2018(JSS)、Lacerda et al. 2020 の結論、Buse & Weimer 2010、Scalabrino の readability モデル(2016/2018)。
- Lavazza, Morasca & Gatto 2023(EMSE)の結果。
- TU/e と Pharo の閾値検証の結果。
- arXiv 2602.18579 の結果(タイトルのみ確認)。
- arXiv 2411.05230 の数値。
- Kapser & Godfrey のクローン有害性の研究。
- Google の readability 認定と ML 編集の研究。
- 2022〜2026 年の、CC の LOC を超える予測力の研究、Halstead の検証、MI の検証、ROC に基づく閾値研究、スメルと欠陥の効果量を統合したメタ分析、fMRI や EEG による検証。
- rust-code-analysis の CoC 実装が SonarSource 仕様と一致するか(#1249 以外の部分)。

**クローン担当**

- BigCloneEval の型別 recall 表(NiCad、SourcererCC、CCFinderX、Deckard、iClones、CloneWorks、Oreo、CCAligner、PMD-CPD、Simian):数値は確認していない。
- CCT-Code、ZC3、対照学習による事前学習、2022〜2026 年のグラフ/PDG 系検出器、NiCad の現在の文法一覧(TXL の Rust 文法の有無)、Deckard の保守状況、Sourcegraph の重複検出。
- Moumoula et al. FSE 2025(タイトルのみ)、CRec の precision、arXiv 2502.04073、LessLeak-Bench の対象データセット。
- GPTCloneBench や CodeNet が学習データ汚染でどれだけ水増しされているか。
- min tokens と precision の関係を測った研究(どの言語についても見つからず、jscpd についても見つからなかった)。
- 開発者がクローンのリファクタリング推薦を受け入れる率。
- ICSE 2026 のクローン系譜研究は計画段階のもので、結果がない。

**履歴・アーキテクチャ担当**

- [prior knowledge] で再取得しなかったもの:Rahman & Devanbu 2013 の効果量、Kamei et al. 2013、McIntosh & Kamei 2018、Herzig & Zeller 2013、Decoupling Level(ICSE 2016)。
- 再取得しなかった基礎論文:Gall et al. 1998、Zimmermann et al. 2004/2005(ROSE)、D'Ambros et al. 2009。
- 取得できなかったもの:Kovalenko et al. ASE 2018。
- 調べなかったもの:Tornhill の書籍にあるパレート型の集中の数値、Verdecchia et al. などの ATD 二次研究、循環の効果量(LOC を統制したオッズ比など)、Designite・Sonargraph・Structure101 の現状。
- 変更の散らばりや波及効果のツールは一次資料で確認していない。arXiv:2607.23355 は読んでいない。
- 調べなかったもの:ra_ap_* を依存抽出に使う方法、churn 系 CLI、cargo-depgraph の移転先。

**LLM 担当**

- 名前付きの静的検出器(PMD、DesigniteJava、SonarQube)との、検証可能な precision/recall の比較表。
- RefactorBench、Shirafuji et al.、Liu et al. の数値、iSMELL のリファクタリングの正しさ。
- arXiv 2605.21453、2604.16790、2505.16222、2510.22409 の数値。
- Google の 96 名 RCT(二次資料のみ)。
- Meta や Microsoft の定量報告、GitHub Copilot code review・CodeRabbit・Graphite・SonarQube AI Code Assurance の方法が記述された数値。
- Claude Code や Devin の出力の保守性を対象にした統制研究。
- arXiv 2607.09979 の日付と venue。

**Rust ツール・振る舞い保存担当**

- Lizard が Rust に対応するか。CodeScene、Qodana、Understand、Semgrep の Rust 対応の成熟度。SonarQube の RSPEC-3776 の既定閾値と、#1249 の事例での挙動。
- Clippy の lint ごとの誤検知率と、arXiv 2310.11738 の回答者数。
- rust-analyzer の extract/inline assist が意味的に何を保証するか。
- Evans 2020、Astrauskas 2020、Rudra 2021 の数値の一次資料。
- Rust でのミューテーションスコアと欠陥の関係、cargo-mutants のスループット(`performance.md`)、nextest の retry・JUnit・タイムアウト機能の再確認、Rust のテストスメルや flaky テストの研究、回帰テスト選択ツール、LLM による特性テストの研究。
- arXiv 2309.02395、arXiv 2607.22161(HarnessLLM)、Zhao 2026 の係数、REM 2.0 本文の成功件数。
- 固定版と最新版の差分の中身(jscpd 5.3.1→5.4.2、ast-grep 0.44.1→0.51.0、cargo-modules 0.26.0→0.28.0)。
- cargo-semver-checks、cargo-public-api、cargo-udeps、cargo-machete、cargo-hack、cargo-deny が品質に与える効果の研究(状態は ★ で確認したが、機能の説明は prior knowledge による)。
