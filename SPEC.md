# pdfdelta 技術仕様書 v5

2026-09-20改訂：日本語の15章構成（v4まで）を復元し、現行実装の証拠チャネル、共通solver、局所失敗、安全境界を各章へ統合した。
本文書は英語版 `comparison contract v5`（`3586485`、2026-09-10）を日本語の章立てへ戻した更新版であり、タイトルのv5は比較契約の版を指す。
主評価は可視PDF-native文字であり、画像・パス文字・OCRの新規実行は現行スコープ外である。
実装済みの範囲と将来目標を章ごとに区別して記す。

## 1. 目的

二つのPDFを比較し、文書として意味のある変更だけを検出するCLIツールをRustで実装する。

```bash
pdfdelta old.pdf new.pdf
```

旧版の `Release 10 remains available.` が新版で `Release 20 remains available.` になっていれば、これを1件の置換として検出する。
一方、文章内容が変わらないまま改行位置、改ページ位置、フォントサイズ、余白、段組、生成ソフトウェアだけが変わった場合は、変更に数えない。

検出対象はPDF内部表現の差ではなく、人間が文書として認識する内容の差である。
比較は証拠に基づいて行い、著者の意図や画面上の完全一致を証明するものではない。

### 1.1 設計の骨格

全体は次の思想で組む。

```text
Raw PDF Evidence → Imperfect Structure → Robust Alignment → Exact Diff
```

- 構造化（Line/Block復元）に100%の精度を要求しない。構造化の誤りはAlignmentで救う。
- Alignmentでは多少の文字変更を許容して対応付ける。
- 最後のExact Diffだけは変更をごまかさない。`Release 10` → `Release 20` のような小さな変更を消さない。
- どの段階でも、確信が持てない場合は誤った結果を返さずUNRESOLVEDとして報告する。

### 1.2 普通のdiffが使えない理由

PDFは論理的な文章構造を保存する形式ではなく、「この文字をこのフォントでこの座標へこの順番で描画する」という命令の列に近い。
そのためplain text抽出とdiffの組み合わせは次の点で壊れる。

- **改行**：同じ文が2行に分かれているだけで差分になる。
- **改ページ**：同じ段落がページ境界を跨いで移動しただけで差分になる。
- **段組**：左右2列の読み順は文書種別で異なる。論文なら左列から右列へ読むが、法律の新旧対照表なら左右が行単位で対応する。左右に分かれているという情報だけから読み順を決めてはならない。
- **構造化の揺れ**：同じ文章でも版によってBlock分割が1個になったり2個になったりする。完全な文書構造を前提にした設計は成立しない。

### 1.3 主評価と現在の達成状況

主評価は、可視のPDF-native文字を比較する`--native-text-only`経路で、固定36ペアの公開改訂ペアを比較完了できるかである。
最終binaryでは少なくとも3/36ペア（Schedule SE、および片側に可視native文字を持たない独立producer 2ペア）が同一入力・同一引数・既定budgetで2回とも同一reportで完了することを確認した。
36ペア全体の最終binaryでの再測定は行っておらず、完了数を36/36とは主張しない。

既定の証拠チャネル経路（text, visual, forms, relations）は、未知PDFに対する比較契約の移行目標である。
チャネルごとのinventoryと比較義務がすべて解消して初めて文書比較は完了する。
現行実装はこの目標の一部を実装しており、全PDFの完全比較を達成したとは扱わない。

## 2. スコープと完成条件

### 2.1 最初の対象

born-digital PDFを中心に、可視のnative文字を比較する。

| 扱う | 扱わない |
|---|---|
| 日本語・英語（初期完成条件は横書き） | 縦書きの完全対応、手書き |
| 1段組の複数ページ文書 | AcroForm全般の解釈、高度なannotation |
| ToUnicodeあり・なし双方の基本フォント、Unmapped token | 複雑な表の完全解釈、画像内容の意味比較 |
| xref stream / object streamを含む現代的なPDF | PDF 2.0固有機能の網羅 |
| empty user passwordまたは明示passwordで復号できる暗号化PDF | password不明の暗号化PDF、未対応security handler |
| 保存済みAcroForm値、widget crop、明示的な参照edge | XFA、export option解釈、link/footnote targetの自動解釈 |

非対応機能に遭遇した場合は、誤った結果を返すのではなくUNSUPPORTED / UNRESOLVEDとして扱う。

画像とパス文字は主評価の対象外であり、画像は復号済みpixel hashによる粗い比較だけを行う。
既存の可視OCR文字層はnative glyphとして保持し、可視content比較の対象にする。
非描画text（render mode 3等）は従来どおり可視content比較の対象外とする。
OCRの新規実行、model loading、手書き認識は現行スコープ外であり、実装しない。これは恒久的な禁止ではなく、§10.3の証拠契約に沿う追加providerの将来課題として扱う。

### 2.2 最初の完成条件

次の5ケースを確実に解いた時点を最初の実用版とする。この条件は変更しない。

| Case | 内容 | 期待値 |
|---|---|---|
| 1 | 改行位置だけ違う | 0 changes |
| 2 | 改ページ位置だけ違う | 0 changes |
| 3 | Release 10 → Release 20 | 1 replacement |
| 4 | 段落追加 | 1 insertion |
| 5 | 段落削除 | 1 deletion |

Case 1は英語文書も含む。英語PDFはspace glyphを描画せず座標移動でspaceを表現することが多いため、space再構成（§7.1）がこの完成条件の前提になる。
二段組と表はこの時点の完成条件に含めない。

### 2.3 証拠チャネルとスコープの宣言

比較対象はチャネルとして選択する。

| channel | 比較する内容 | 現状 |
|---|---|---|
| text | 可視native glyphの内容 | 実装済み（§6、§7、§8、§9、§10） |
| visual | 埋め込み画像のpixel hashと配置 | 実装済み（粗い比較） |
| forms | 保存済みfield値、widget crop、宣言appearance state | 一部実装済み |
| relations | 呼び出し側が供給する参照edge | 一部実装済み（自動抽出は未実装） |
| presentation | 改行、改ページ、座標、font metrics | 観測のみ。既定の内容変更に数えない |

既定チャネルはtext, visual, forms, relationsであり、`--channels`で明示的に選ぶ。すべての`--channels`実行は共通証拠pipelineを使う。
`--native-text-only`はnative glyph専用の旧契約（report schema 11）を維持し、`--channels`とは併用できない。

選択したチャネルが欠落または未検証である場合、その比較は不完全である。
providerの結果が無いことは「内容が空」ではなく「未取得」である。
空のinventoryが完全であると言えるのは、そのチャネルとscopeを実際に検査したproviderがある場合だけである。

### 2.4 完了・未解決・失敗の区別

- **完了**：選択した全チャネルで、各側のinventoryが完全であり、比較義務（対応付けと局所比較）が解消している。
- **未解決**：証拠は保持しているが、対応や位置を確定できない。候補、競合、探索打ち切り、抽出gapを含む。
- **未対応**：解釈自体が実装されていない（例：一般のlink target、XFA、画像内文字の認識）。
- **実行失敗**：I/O、malformed input、fatal backend error、資源制限で結果を表現できない。

欠落した証拠は、無関係な領域を不必要に無効化しない一方で、隠れた競合やglobal uniquenessを無効化し得る。
局所性は隠れた競合を無視する許可ではない。
独立して有効な局所結果は、比較が不完全でもreportに残す。

## 3. アーキテクチャ

### 3.1 パイプライン

```text
PDF bytes
  │
  ▼ PDF Parser Backend        … object / xref / page / stream access
  │
  ▼ Primitive Extraction      … Content Stream interpreter → Glyph + geometry
  │
  ▼ Evidence Store            … native / rendered / structured evidence
  │
  ▼ Document Graph Views      … Line / Block / Region / tags / fields / edges
  │
  ▼ Shared Correspondence     … old ↔ new の対応候補と所有権
  │
  ▼ Local Comparisons         … exact diff / value / relationship / visual
  │
  ▼ Changes / Candidates / Coverage / Unresolved
```

PDF構文とobject graphの解決、および描画命令からGlyphを復元する処理を分離する。
既存parser libraryを使っても、lossyなplain text extractorへ依存しない。
後段が必要とするraw object、Content Stream、Resources、object idはparser境界越しに保持する。

各段は独立させる。Layout Reconstructionが間違ってもAlignmentで救い、Alignmentが不確実ならUNRESOLVEDへ落とす。
構造と対応は局所的に互いを精錬できるが、精錬はviewを選ぶだけであり、差分を小さくするために元の文字、数値、画像、field値を書き換えない。

### 3.2 PdfParser境界とGlyphExtractor境界

PDF object parsing専用のtraitと、Glyph抽出専用のtraitを別々に置く。

```rust
pub trait PdfParser: Send + Sync {
    fn parse(&self, pdf: Arc<[u8]>, limits: ParseLimits)
        -> Result<Box<dyn ParsedPdf>>;
    fn parse_with_password(&self, pdf: Arc<[u8]>, limits: ParseLimits, password: &str)
        -> Result<Box<dyn ParsedPdf>>;
}

pub trait ParsedPdf: Send + Sync {
    fn version(&self) -> PdfVersion;
    fn trailer(&self) -> Result<PdfDict>;
    fn resolve(&self, reference: ObjectRef) -> Result<PdfObject>;
    fn pages(&self) -> Result<Vec<PageRef>>;
    fn page_dict(&self, page: PageRef) -> Result<PdfDict>;
    fn raw_stream(&self, reference: ObjectRef) -> Result<RawStream>;
    fn decoded_stream(&self, reference: ObjectRef) -> Result<DecodedStream>;
    fn issues(&self) -> &[PdfIssue];
}

pub trait GlyphExtractor: Send + Sync {
    fn extract(&self, pdf: &dyn ParsedPdf, limits: ExtractionLimits)
        -> Result<Document<Glyph>>;
}
```

`PdfVersion`、`ObjectRef`、`PdfObject`、`PdfDict`、`RawStream`、`DecodedStream`、`ParseLimits`、`ExtractionLimits`はpdfdelta側の中立型とし、`lopdf::Object`のようなlibrary固有型を後段へ漏らさない。
`ParsedPdf`は完全なPDF DOMの再設計ではなく、Primitive Extractionに必要な最小capabilityを安定化するfacadeである。

production pipelineでは`PdfParser`とpdfdelta独自の`GlyphExtractor`を`ParserBackedGlyphSource` facadeで合成し、PDF bytesから`Document<Glyph>`までを提供する。
Diff engineの開発は、PDFを経由せずprogrammatically構築した`Document<Glyph>` fixtureで進められる。
既存text extractorは主backendにせず、Differential Testingのoracleとしてのみ使う。

### 3.3 既存parserを先に使う方針と自作backendの条件

PDF object parserは自作しない。Header、lexer、indirect object、xref、object stream、trailer、Page Tree、stream decodeは既存libraryへ委譲し、独自実装は差分に必要な証拠を失わずGlyphへ復元する処理から始める。
Content Stream / Text State interpreterは自前で持つ。既存のplain text extractorはrender order、文字単位のgeometry、raw code、Resourcesの由来を捨てることがあり、Exact Diffまで追跡可能な証拠として不足するためである。

自作`PdfParser` backendは、次のいずれかをfixtureまたはbenchmarkで確認した場合にだけ追加する。

- 必須scopeのxref stream、ObjStm、incremental update、resource inheritanceを既存backendが扱えず、adapterまたはupstream修正で解決できない。
- Content Streamのraw bytes、object identity、filter情報など、後段に必要な証拠を取得できない。
- malformed PDFへのresource limitやerror分類を既存backend上で安全に実装できない。
- Differential Testingで既存backendの誤りが再現し、代替backendでも要件を満たせない。

自作backendを追加しても`PdfParser` / `ParsedPdf`境界と上位pipelineは変更しない。backend切り替え自体をロードマップの既定ゴールにしない。

### 3.4 Workspace構成

```text
pdfdelta/
├ Cargo.toml
├ crates/
│  ├ pdfdelta-core/        # pure library。CLIを知らない
│  │  └ src/
│  │     ├ pdf/            # backend / object / content / font
│  │     ├ source.rs       # ParserBackedGlyphSource、fixture、抽出境界
│  │     ├ model/          # Glyph等の中立型
│  │     ├ layout/         # Line / Block / geometry
│  │     ├ alignment/      # anchor / candidate / score / ordered
│  │     ├ diff/           # Myers、assessment、recovery
│  │     ├ document/       # 証拠store、graph、channel、共通solver、局所比較
│  │     └ report/         # text / JSON report
│  ├ pdfdelta-cli/         # 入出力、password、worker、limit、publication
│  └ pdfdelta-bench/       # fixture生成、mutation、評価、pdfbench
├ fixtures/                # external / extraction-conformance / issue12 / issue20 等
├ benchmark/
│  ├ manifests/ expected/  # 生成benchmarkのmanifestと期待値
│  └ realworld/            # 公開改訂ペアのpanel、baseline、results
└ fuzz/                    # このcrateのみnightly許容
```

crate名とバイナリ名の対応は次の通りである。

| crate | 種別 | バイナリ |
|---|---|---|
| `pdfdelta-core` | lib | — |
| `pdfdelta-cli` | bin | `pdfdelta` |
| `pdfdelta-bench` | bin | `pdfbench` |

CLIはfilesystem、password、worker、limit、report publication、process statusを所有する。
coreはvalidation、graph view、correspondence、局所比較、report modelを持ち、filesystemへ触れない。
benchは生成入力、mutation、独立annotation、manifest、評価を所有する。

### 3.5 外部プロセスの隔離

Linuxでは、native証拠取得、rendering、画像hashingを境界付き子プロセスで実行する。
子プロセスはaddress space、CPU時間、wall-clock、入出力サイズ、page数、pixel数、occurrence数の上限を持ち、親が入力hash、page identity、acquisition role、証拠、集計上限を検証してから比較へ渡す。
終了したworkerの欠落を、画像の追加・削除へ読み替えない。
Windows等、子プロセス制限を実装していないplatformでは、その事実を明示的なunsupportedとして扱う。

### 3.6 言語と依存の方針

stable Rust、Rust 2024 Editionを前提とする（例外はfuzz crateのみ）。

**自前実装するもの**：parser library adapterと中立facade、Content Stream / Text State解釈、ToUnicode/CMapの必要部分、Glyph geometry復元、GlyphからLine/Block/Regionへの構造化、canonical/matching規則、soft line break policy、normalization event記録、anchor検出、candidate generation、alignment（1:N/N:1、move検出、coverage算出）、共通correspondence solver、assessment、Myers diff。

**production dependencyとして使ってよいもの**：PDF object parser backend（現在`lopdf`）、CLI parsing（`clap`）、JSON（`serde` / `serde_json`）、Unicode character data（`unicode-normalization`、`unicode-segmentation`、`unicode-bidi`）、hash（`sha2`）、小容量vector（`smallvec`）、stream decode（`flate2`）、描画（`hayro`、CLIのvisual channel）、CID CMap参照data（`hayro-cmap`）。
Unicode crateへ委ねるのは文字dataとNFC/segmentation primitiveに限り、canonical/matching規則、soft line break policy、normalization event記録は自前実装に保つ。

**dev / test dependency**：`proptest`、比較oracle用の外部extractor、fixture生成用の外部renderer。
production設計をoracleの出力形式へ合わせない。

## 4. データモデル

### 4.1 Glyph

```rust
pub struct Glyph {
    pub id: GlyphId,
    pub text: DecodedText,
    pub raw_code: Vec<u8>,
    pub page: PageId,
    pub bbox: Rect,
    pub baseline: Vec2,
    pub direction: Vec2,
    pub font_id: FontId,
    pub font_size: f64,
    pub render_order: u32,
    pub render_mode: TextRenderMode,
    pub provenance: GlyphProvenance,
}

pub struct GlyphProvenance {
    pub content_stream: ObjectRef,
    pub operator_index: u32,
}
```

座標と行列はf64を使う。行列積の累積誤差を考えるとf32を選ぶ理由がない。
座標は`/Rotate`適用済みのcanonicalページ座標系で格納し、ページ回転の正規化を後段に持ち込まない。
`bbox`はink outlineではなく、font width、ascent/descentと変換行列から求めるlayout bounding boxとする。rotated textでは`baseline`と`direction`も併用し、axis-aligned `Rect`だけで順序を決めない。

`render_mode`と`provenance`は、invisible textや上書き描画を後から検証し、backend間の差をobject/operator単位まで追跡するために保持する。
初期stageで完全なvisibility判定を行わなくても証拠を捨てない。

```rust
pub enum DecodedText {
    Mapped(String),
    Unmapped { font_hash: FontProgramHash, glyph_id: u16 },
}
```

Unicodeへ戻せないglyphをU+FFFDや空文字で潰さない。
born-digital PDFでもsubset fontでToUnicodeを持たないものがあり、一律UNRESOLVEDに落とすとcoverageが実用にならないためである。
old/newが同一font programを埋め込んでいれば`(font_hash, glyph_id)`を比較tokenとしてmatchingとdiffが成立する。
font programが異なりUnicodeにも戻せない場合はUNRESOLVEDとする。

### 4.2 可逆な構造化と文字列view

構造化は不可逆変換にしない。

```rust
pub struct Line  { pub glyphs: Vec<GlyphId>, /* … */ }
pub struct Block { pub lines: Vec<LineId>,  /* … */ }
```

Block分割を間違えた場合に元のGlyph列へ戻って再解析できることが、Alignment側での救済の前提になる。
native blockは不可分なground truthではなくviewとして扱う。

```rust
pub struct BlockText {
    pub raw: MappedText,
    pub canonical: MappedText,
    pub matching: String,
    pub normalization_events: Vec<NormalizationEvent>,
}

pub struct MappedText {
    pub text: String,
    pub source_map: Vec<SourceMapEntry>,
}
```

`ScalarRange`はUTF-8 byte offsetではなくUnicode scalar valueのindexで定義する。
ligature展開、NFC、hyphenation結合で1 glyphがN文字またはN glyphが1文字になっても、canonical範囲から元Glyphとgeometryへ戻れることを必須とする。
`matching`はExact DiffやChange spanに使わないため、完全なsource mapを要求しない。

### 4.3 変更と比較結果

```rust
pub enum ChangeKind { Replacement, Insertion, Deletion, Move }

pub struct Change {
    pub kind: ChangeKind,
    pub old_span: Option<TextSpan>,
    pub new_span: Option<TextSpan>,
    pub confidence: Confidence,
    pub tags: Vec<ChangeTag>,
}
```

`TextSpan`はBlock集合、複数Blockを結合した際の`BlockSeparator`、canonical文字範囲、comparable token範囲を保持する。
単一Blockのspanはseparatorを持たない。複数Blockのspanは`Concatenate`、`Space`、または境界ごとの明示patternを必要とする。
変更の単位は対応付いたBlock集合の上のcanonical文字範囲であり、範囲indexはUnicode scalar indexである。

結果は次のように分離する。

| 概念 | 内容 |
|---|---|
| `Comparison.changes` | 位置と種類を確定した内容変更 |
| `Comparison.change_candidates` | 推測した編集。原文範囲、競合グループ、未確定理由、必要なら順位を保持し、打ち切りを明示する |
| `Comparison.proven_changed_regions` | 内容の不一致は確定しているが位置を特定できない領域 |
| `Comparison.unresolved_regions` | 上記以外の未解決内容。候補がある範囲を含む |
| review unit | 確定した対応の上でのcount query（変更token数の下限・上限、全optimal alignmentで変更される位置） |

各側で、抽出済みcomparable tokenを同一、変更、未解決へ重複なく分割する。

```text
抽出済みcomparable token数
  = 同一と確定したtoken数 + 変更と確定したtoken数 + 未解決token数

解決済みcoverage
  = (同一 + 変更) / 抽出済みcomparable token数
```

件数はevent spanの単純合計ではなくtoken区間の和集合から求める。
確定した挿入と削除は内容がある側を解決済みにし、零幅の境界はtoken数を消費しない。
候補と位置未確定の変更領域は、位置が確定するまで分子に含めない。
Formatting-onlyは内容の解決状態と直交し、coverageを増やさない。
抽出が不完全な場合は`extraction_complete: false`と問題の範囲を併記し、量が不明な欠落を0 tokenとして扱わない。分母が0の場合は百分率を表示しない。

### 4.4 証拠store、合同契約、source reference

証拠はimmutableなstoreに保持し、native、rendered、structuredのsource referenceを区別する。

- native referenceはglyph text、raw code、font identity、geometry、baseline、direction、render order/mode、crop/clip観測、vector line、PDF object/content-stream/operator provenanceを保持する。
- rendered referenceはpage/region geometry、polygon、pixelまたは検証可能なimage identity、backend/version/profileを保持する。
- structured referenceはfield、tag、annotationのidentityと、可能なら元objectを保持する。

同じ可視内容のnative版とOCR版は、二つの独立contentではなく等価または競合するviewとして扱う。
等価性の根拠を記録し、証明されない重複は競合として保持して二重消費しない。
OCRは常にimage referenceと認識候補を保持し、native glyph IDを捏造しない。

比較契約は選択channelの集合とversionを保持する。backend identityはkind、name、version、profile、modelを保持し、reportへ残す。

### 4.5 Document graph

section、paragraph、list/item、table/row/column/cell、form/field、figure/caption、annotation、code、mathematics、unknown regionを、source-backed nodeまたはcandidate viewとして表現する。
typed edgeはcontainment、local order、row/column identity、label、caption、reference、alternativeを保持する。
座標とtext directionはevidence側に留める。

tag、geometric reconstruction、render order、modelは前提が異なる。
tagの存在はsource factだが、そのreading orderやsemantic roleは競合し得る。model scoreは証明ではない。
dangling reference、malformed geometry、hierarchical relationのcycleを拒否する。

## 5. Change スキーマと出力

### 5.1 判定とconfidence

`Confidence`は判断の補助情報であり、正解確率や単独の確定条件として扱わない。
`High`や`TrustedRun`という名称だけを確定の根拠にしない。
確定とは、抽出、正規化、構造、探索に関する明示した前提のもとで比較の確定条件を満たした状態を指す。
誤って対応付けた段落の内部でexact diffが計算できても、その差分は正しくない。

### 5.2 出力カテゴリ

出力は、位置まで確定した変更、位置未確定の変更領域、差分候補、formatting-only、未解決領域に分ける。
候補には`TENTATIVE`を明示し、未確定理由と原文位置を表示する。
候補件数は確定変更件数に含めず、候補範囲を解決済みcoverageへ加算しない。

canonical正規化で吸収した差も黙って消さず、formatting-onlyとして独立に報告する。
このカテゴリはbest-effortであり、0件でもrenderingが完全同一であることは保証しない。content change判定やexit codeには影響させない。

### 5.3 人間向けtext report

text reportはexact diffの結果をreviewしやすい文脈付きunified diffへ投影する。
これは表示層のみの変換であり、`Comparison`やJSON reportの機械可読な意味を変えない。

```text
established changes: 1 · changed regions: 0 · tentative candidates: 1 · formatting-only: 3 · unresolved regions: 2 · extracted-token coverage 97.4% · incomplete

--- old.pdf
+++ new.pdf

@@ page 1 · old block 12 -> new block 14 · confidence: medium @@
- ... Form 1040 (2024) ...
+ ... Form 1040 (2025) ...

@@ page 3 · UNRESOLVED @@
? could not safely align this region (evidence: text_similarity)

@@ page 3 · TENTATIVE · candidate group 1 @@
? possible replacement; competing reading orders remain
```

要約行には全出力カテゴリとcoverageを1行で併記する。
`---` / `+++`のfile header、`@@ page N ... @@`（page番号は1-based）のhunk header、`-` / `+`の変更行、有界なcontext、`~ moved`、未解決の`?`行を使う。
PDF由来のtextは制御文字とUnicode bidi制御文字をliteralな`\u{...}`へescapeし、hostileなglyph mappingがterminalを操作できないようにする。
ANSI色は`--color auto|always|never`で制御し、既定の`auto`はstdoutがterminalの時だけ着色する。色は記号の補助であり、色なしでも読める。
「No differences found」とだけ表示することはない。

### 5.4 JSON reportとtrace

JSON reportはtext reportより多くの情報を保持する。
確定変更、候補、位置未確定の変更領域、formatting-only、未解決領域、source ownership、relation dependency、assumption、探索完全性、work消費、抽出完全性、source coverage、review unitを含む。

| report | schema | 内容 |
|---|---|---|
| native report（`--native-text-only`） | 11 | glyph、block、change、assessment、extraction issue |
| channel report（既定`--channels`） | 2 | 選択channel、証拠store view、graph、solver結果、局所比較 |
| trace（`--trace-json`） | 26 | phaseごとのbounded diagnostics、typed limit error、skipしたphase |

traceの`duration_us`はrunごとに非決定的であり、golden-file比較から除外する。
`--json`と`--trace-json`、`--output`はそれぞれ新しいpathへatomicにpublishし、既存fileと入力PDFを上書きしない。
second publicationが失敗した場合、first artifactは残し、対になるrollbackは行わない。
JSONの変更有無と比較完全性は独立した値であり、`detected`、`indeterminate`、`no_content_change`は抽出・比較の完全性とは別に扱う。

### 5.5 CLI と exit code

```bash
pdfdelta inspect document.pdf
pdfdelta inspect document.pdf --backend-info
pdfdelta inspect document.pdf --objects
pdfdelta inspect document.pdf --glyphs
pdfdelta inspect document.pdf --svg overlay.svg
pdfdelta old.pdf new.pdf
pdfdelta old.pdf new.pdf --channels text,visual,forms,relations
pdfdelta old.pdf new.pdf --native-text-only -j result.json
pdfdelta old.pdf new.pdf -o diff.txt -q -s
pdfdelta old.pdf new.pdf --review review-output
pdfdelta old.pdf new.pdf --color auto|always|never
pdfdelta old.pdf new.pdf --limit-scale 16
pdfdelta old.pdf new.pdf --old-password-file old.secret --new-password-file new.secret
pdfdelta old.pdf new.pdf --old-font-identity FontName=identity --new-font-identity FontName=identity
pdfdelta old.pdf new.pdf --extraction-cache-dir ~/.cache/pdfdelta
pdfdelta completions bash|zsh|fish|powershell|elvish
```

`inspect`はbackend summary、object、glyph、SVG overlayを提供する。
比較CLIはtext report、JSON、trace、review bundle、quiet、strict alias、color、limit scale、side別password file、side別external font identity、extraction cacheを持つ。
passwordはargvへ直接渡さず、sideごとのpassword fileから読み、本文をerror、report、traceへ書かない。
`--old-font-identity` / `--new-font-identity`は`BaseFont=identity`形式のcaller assertionであり、identity文字列はdomain-separated hashへ変換した後は保持しない。同じ外部font programを使うとcallerが保証できる場合だけ指定する。
`--limit-scale`は比較pipelineのbudgetだけを拡大し、parserとextractionのlimitは変更しない。

exit codeはCI利用を前提に定義する。

| code | 意味 |
|---|---|
| 0 | 比較が完全で、確定した内容変更がない |
| 1 | 比較が完全で、確定した内容変更がある |
| 2 | 実行不能なerror（I/O、fatal parse error、report書き込み失敗等） |
| 3 | 結果を返せるが比較が不完全。候補、未解決範囲、抽出欠落、位置未確定の変更領域を含む |

判定優先順位は`2 > 3 > 1 > 0`とする。不完全な比較は、確定した変更があっても既定で3を返す。
`--strict`は新しい既定動作の互換aliasとして受理し、quiet modeにも同じ終了条件を適用する。
coreは比較結果の状態を返し、process exit codeへの変換はCLIが所有する。
text比較の完全性は画像比較や見た目の一致を意味しない。

## 6. PDF Backend と Primitive Extraction (Track A)

### 6.1 目的

PDFからplain textを取り出すことではなく、後から検証可能なraw evidenceを保ったまま、文字、描画位置、描画順を復元することが仕事である。

### 6.2 既存PDF Parser backend

初期版は既存parser libraryを`PdfParser` adapter越しに使用する。現在のdefault adapterは`lopdf` 0.45.0である。
必須capabilityは次の通りである。

- Header / version、indirect object、reference解決。
- classic xref tableとxref stream、object stream、trailer chainとincremental updateのlatest revision解決。
- Page Tree走査とResourcesの継承。壊れた枝にParent不整合または参照循環があっても、独立して検証できた正常枝は保持し、欠落枝とroot `Count`不一致をdocument-scoped UNRESOLVEDとして返す。
- raw stream metadataとdecoded stream bytesの取得。
- FlateDecode。未対応filterは空文字へ潰さずUNSUPPORTEDにする。
- 暗号化の検出。empty user passwordの自動復号、またはcallerが明示passwordを借用して復号できた場合だけ受理する。password欠落・不一致、復号後も`Encrypt`が残る文書、未対応security handlerはUNSUPPORTEDにする。
- object count、recursion、decoded size等のresource limitとerror categoryの保持。

adapterはobject id、generation、dictionary key、stream filter、Content Stream順序を保持する。
library独自型は`pdf/backend/`内で中立型へ変換し、後段へ漏らさない。
backendの選定は人気ではなく、fixture通過率、raw evidenceへのaccess、安全なlimit実装可能性で決める。

### 6.3 Content Stream interpreter と座標

Content Streamの構文解釈とText / Graphics State追跡はpdfdelta側で実装する。
扱うoperatorは次の通りである。

```text
q Q cm
BT ET Tf Tm Td TD T* TL Tj TJ ' " Tc Tw Tz Ts Tr
Do
```

`q` / `Q` / `cm`でGraphics StateとCTMを追跡する。
`Do`ではXObject subtypeを解決し、ResourcesとMatrixのstackに上限を設けた上で、Form XObjectだけを再帰的に解釈する。
Formは呼び出し元から分離したstate snapshotで実行するため、対応する`Q`がない`q`はForm境界でのみ破棄してよい。一方、`Q`のunderflowとPage単位のstack不均衡はUNRESOLVEDのままにする。
Image XObjectは画像比較が主scope外であるため明示的にskipする。`'`と`"`は対応する複合text operationへ展開してから解釈する。
`BI` / `ID` / `EI`は専用inline-image lexerで処理し、後続operatorから解釈を再開する。

Pageの`/Contents`が複数streamのarrayである場合は指定順に解釈し、完成済みtokenをstream境界で連結しない。
pending operandはarray全体で保持し、dictionary keyのvalueが次のstreamから始まる場合だけ限定的に回復する。
未完成operandのbufferと再parseは1回に制限し、次のstreamでも完成しなければ同じprefixを繰り返し処理せずUNRESOLVEDとする。

Page boxは`/Rotate`を適用する前に各axisを`min`と`max`で正規化する。
有限な座標の大小が逆転している場合は受理し、面積がゼロまたは非有限のboxはUNRESOLVEDとする。

CTM、Text Matrix、Text Line Matrix、Font Matrix、horizontal scaling、character / word spacing、text riseを追跡し、最終Glyph座標とadvanceを求める。
Text Render Modeは初期から記録する。
non-painting modeのGlyphはevidenceとして保持するが、可視content比較からは除外する。
clipping、alpha、白塗り上書き等を含む完全なvisibility判定は後続stageで扱う。

### 6.4 Font decoding

glyph codeからUnicodeへの変換は単純ではない。ToUnicode、simple font、Type0 font、CIDを順次扱う。
`pdf/font/` moduleとして分離し、codeの分割、mapping、advanceを一つの結果として返す。

```rust
pub trait FontDecoder {
    fn decode_codes(&self, bytes: &[u8]) -> Result<Vec<DecodedCode>>;
}

pub struct DecodedCode {
    pub raw_code: Vec<u8>,
    pub text: DecodedText,
    pub glyph_id: Option<u16>,
    pub advance: f64,
}
```

現在の対応範囲は次の通りである。

- simple fontではcodeごとに完全一致するToUnicode entryを優先し、entryが存在しない場合だけDifferencesから宣言済みのStandard、WinAnsi、MacRoman encodingの順にfallbackする。明示された不正entryと未知のDifferencesは`Unmapped`のまま保持し、fallbackしてはならない。
- Standard 14 fontではcanonicalなBaseFont名と、組み込みencodingまたは明示されたStandard / WinAnsi / MacRoman encoding名をstable identityへ含める。Differences dictionaryはcode selectorの意味を変えるため、このcanonical identityの対象にしない。
- Type1Cのidentityは、subtypeが`Type1C`である単一の`FontFile3` streamを必要とする。MMType1はvariation axisを解釈せず、宣言済みsimple-font encodingとmetricsだけを使い、unmapped glyphへstable identityを与えない。
- 定義済みIdentity-HとIdentity-Vは常に固定2-byte codeとしてcontentを分割する。custom Type0 Encoding CMapは、`WMode`が0または1で、単一のfull-domain codespaceと、同じ範囲をCID 0から写す単一のidentity `begincidrange`だけを持つ場合に限り、固定1-byteまたは2-byte codeとして扱う。`usecmap`、`begincidchar`、複数range、非identity mappingはUNSUPPORTEDとする。
- ToUnicodeはdecoderの固定幅で完全一致参照する。entry欠落、孤立したUTF-16 surrogate destination、ToUnicode自体の欠落は`Unmapped`とする。一方、空、奇数長、非hexのdestinationはerrorのままにする。
- 埋め込みfont programもToUnicodeもないCID fontは、BaseFont名だけでglyph同一性を推測しない。ただしcallerがsideごとに同じBaseFontへ同じexternal font identityを明示した場合だけ、そのidentityを専用domainでhashし、unmapped glyphのfont identityとして使用できる。これはfont discoveryではなくcaller assertionである。
- Type 3 fontはaxis-alignedな有限`FontMatrix`（`a > 0`、`d != 0`）を持つsubsetだけを扱う。`FirstChar`、`LastChar`、`Widths`、`FontBBox`、`Encoding`、`CharProcs`を必須とし、rotation、shear、translation、horizontal reversalはUNSUPPORTEDのままにする。unmapped glyphのidentityはCharProc streamをglyph nameのbyte順に並べた専用domain hashとし、dictionary順、object ID、Encoding codeの再配置に依存しない。named resourceへ依存する場合は参照先をcanonical graphとしてhashし、循環、深度、byte数の上限で拒否する。
- Identity-Vは、単一のCIDFontType0/CIDFontType2 descendant、固定2-byte code、完全一致参照のToUnicode、per-CID `W2`を持たないsubsetを扱う。`DW2`は省略時の`[880 -1000]`またはdownward displacementを持つ有限な2要素arrayを受理する。一般のcustom vertical CMap、per-CID `W2`、一般のvertical reading orderはscope外とする。

CID descendantの`FontDescriptor`でAscent / Descentが欠落するか`Ascent <= Descent`となってcross-axis extentを構成できない場合は、妥当な`FontBBox`のtop/bottomをfont-wide vertical metricとして使う。どちらからも正のextentを得られない場合だけUNRESOLVEDとし、ゼロ面積Glyphを後段へ渡さない。

### 6.5 抽出の問題分類

抽出の問題はtyped issueとして、document、page、page-tree gap、glyph gapのscopeで報告する。
unsupported（解釈未実装）、unresolved（証拠はあるが確定不能）、resource limit（budget到達）、fatal（結果を表現できない）を区別する。
抽出できなかった量は不明として扱い、空文字や0 tokenへ置き換えない。

### 6.6 資源制限（既定値）

| 境界 | 既定上限 |
|---|---|
| input bytes | 256 MiB |
| objects | 1,000,000 |
| recursion depth | 128 |
| decoded stream bytes | 64 MiB |
| total object-stream bytes | 256 MiB |
| pages | 100,000 |
| glyphs | 5,000,000 |
| Form depth | 32 |
| nesting depth | 64 |
| operators | 5,000,000 |
| stream invocations | 5,000,000 |
| total decoded bytes（抽出） | 512 MiB |
| operand stack | 4,096 |
| array elements | 65,536 |
| operand nodes | 10,000,000 |
| fonts | 100,000 |
| CMap entries | 1,000,000 |
| CID width entries | 65,536 |
| string bytes | 64 MiB |
| vector lines | 5,000,000 |

limitは有限かつ設定可能な状態を維持する。
明示的に低いlimitを指定した場合は同じ課金境界で必ず失敗させる。
default値の再調整は再現可能なfixtureまたはcorpusの証拠に基づく場合だけ行い、文書を通すためにlimit自体を削除しない。

## 7. Layout Reconstruction (Track B)

### 7.1 Glyph → Line

文字進行方向d=(dx,dy)とその垂直方向n=(-dy,dx)を取り、Glyph座標を両方向へ射影してalong-axis / baseline-axis位置として比較する。
data modelは任意方向を保持するが、初期完成条件は横書き文書で評価し、縦書き固有のfont metricsとreading orderは後続benchmarkで追加する。

同一Line候補かどうかは、baseline距離、glyph高さ、フォントサイズ、書字方向、文字間隔から判断する。
固定pixel閾値は使わず、`baseline_distance < 0.25 * median_glyph_height`のような相対値を候補とする。具体値はbenchmarkから決める。

**Space再構成**：Line内で隣接glyph間のalong-axis gapが、フォントサイズと平均advanceに対する相対閾値を超える場合、spaceを挿入する。
space glyphを描画しないPDF（英語文書に多い）でCase 1を解くための必須処理である。再構成したspaceはsyntheticとしてraw sourceと区別する。

### 7.2 Line → Block

Line間の接続score `S = wv·V + wx·X + wi·I + wf·F`を計算する。
V=垂直近接、X=水平重なり、I=インデント類似、F=font連続性である。

page boundaryをBlock boundaryとして固定しない。
前page末尾と次page先頭のLineも、本文領域、indent、font、line gapの正規化値が連続する場合は同一Block候補にする。
繰り返しheader/footerは本文候補から除外し、確信が持てない場合は分割したままAlignmentの1:2 / 2:1へ渡す。

初期段階では文章内容を主判定に使わない。
句読点だけで段落終了を決めず、layout情報を先に使う。page boundary継続の曖昧性を救うための軽い文字種signalは補助として記録してよいが、それだけでBlockを確定しない。

### 7.3 Region

- **XY-Cut**：中央のvertical whitespaceを分割候補とする。column detectedはreading order determinedではない。Region構造とreading orderは別物として扱い、決められない場合はUNKNOWNのまま残す。
- **Region Graph**：TreeではなくAbove/Below/LeftOf/RightOf/Aligned/SameColumnの関係を持つgraphとして保持する。
- **表**：罫線がある場合はvector drawing operatorから`VectorLine { from, to, width }`を取る。罫線なし表はx/y方向の繰り返しalignmentから推定する。表は通常Blockと同じ方法で直列化しない。

rectangular ruled gridはinferred table viewとして、最初の行からcolumn identity、最初の列からrow identityを提案できる。
cell値はaxis correspondenceの根拠にならず、別rowの等しい値でcell対応を確定しない。
header identityが変わった場合は共有child identityまたは隣接identityからinferred候補を作れる。
複雑な表、merged cell、任意のtable semanticsは実装しない。

### 7.4 構造化の可逆性と読み順

行、段落、table cell、tagの境界はsoft境界として保持し、抽出gapや未対応内容の境界はhard境界として区別する。
一つのnative blockを分割・結合したviewは、tokenの所有範囲とseparatorの由来を保持する。
複数の妥当なviewが異なる編集を示す場合は候補として残し、採用viewにかかわらず確定には同じ領域条件を適用する。

読み順は、抽出が完全で対応する単一の順序を裏付けられる場合だけknownとする。
page番号の一致や似た見出しの存在だけではknownにしない。
未知のreading orderに依存する対応は、その前提を保持したまま未確定にする。

## 8. Normalization

一つのBlockから三種類の文字列を作る。ただし、文字列だけを保存してGlyphとの対応を失ってはいけない。
`raw`はPDFから復元したまま、`canonical`は文章として同一とみなす正規化、`matching`はAlignment専用である。

### 8.1 canonical の内容

canonicalで吸収するのは、文章としての同一性に影響しない差に限る。

**吸収する**：Unicode正規化（NFC）、soft line breakの文脈依存join、連続空白の単一化、行末ハイフネーションの結合、ligature展開。

Soft line breakは一律に削除しない。
Latin letter/digit境界では1 spaceを挿入し、CJKおよびCJK/alphanumeric混在境界では連結し、明示whitespaceを重複させない。
行末のU+00AD discretionary hyphenはそのbreakとともに削除できる。
通常の`-`とU+2010は保持する。文字形状や語長からdiscretionary hyphenationを証明できないためである。
字句解釈が不確実な場合、hyphenのsource rangeは未解決のまま残す。段落境界は回復可能なblock境界として残す。

**吸収しない**：全角/半角の差（NFKC相当の互換分解）。
全角半角の統一は識別子、型番、契約番号などで意味のある改訂になり得るため、暗黙に同一視しない。NFKCではなくNFCを採用する理由である。

**独立した解釈根拠**：相手文書と一致する解釈が存在すること自体は、その解釈が正しい証拠ではない。
Exact Diffが消費できるのは独立に正当化されたsource/layout解釈だけである。
Block間の境界は、保持したwhitespace、script規則、source line位置から個別に決める。
候補variantがexact一致しても、joinが未知なら未解決に残す。

canonicalで差を吸収した場合は`NormalizationEvent { kind, raw_range, canonical_range, source }`として記録する。
Alignment後、対応Block集合のold/newでrawは異なるがcanonicalが等しいeventをformatting-onlyへ計上する。
隣接する同種eventは一つへmergeし、Glyph数やLine分割数の違いだけで件数が増えないようにする。
rawに差があったのに出力上なかったことになる状態を作らない。

### 8.2 matching と数値マスク

matchingはAlignmentだけに使う。全角半角の同一視、および`Release 10` / `Release 20`を対応させるための数値マスク（`Release <NUM>`）をここで許可する。

**マスクの安全策**：数値密度の高いBlock（価格表やrelease一覧など）では、マスク後の文字列が行間でほぼ同一になり、誤った1:1対応から「一見正しい誤diff」が生まれる。これを防ぐため次を仕様とする。

- Blockのmatching文字列に占めるマスク由来文字の割合に上限を設け、超えたBlockはマスクなしへフォールバックする。
- マスク一致のみによる対応付けは確定させず、anchor鎖内の位置整合またはneighbor consistencyによる裏付けを必須にする。

matchingはExact Diff、Change span、coverage計算に使わない。

## 9. Cross-document Alignment

candidate generationと最終matchingを分け、近似indexの誤りがそのままChange判定にならない構成にする。

### 9.1 Anchor 検出

一意性が高く確実な一致をAnchorとして探す。候補は長い完全一致Block、一意な見出し、条番号、節番号、表番号、稀な文字列である。
old/new双方に一度ずつしか現れない文字列が強いanchorになる。
anchorの文字列が一意でも、周辺領域の対応や読み順まで自動的に確定するとは限らない。
区間の確定には境界、競合、探索範囲の条件を適用する。

### 9.2 Anchor の順序整合と move 候補

oldからnewへのanchor対応列に対し、new側indexのLongest Increasing Subsequenceを取り、main chainとする。
局所的に同じ文字列が現れても、文書全体の順序から不自然な対応を除外できる。
LISから外れたanchorはmove候補として保持する。

`ChangeKind::Move`の確定には、内容の完全一致、出現箇所の曖昧さのない対応、移動を示す前後の順序関係を必要とする。
scoreだけではmoveを確定しない。根拠が不足する場合はmove候補または未解決領域として残す。
deletionとinsertionに分けて確定する場合も、それぞれの対応領域が確定条件を満たす必要がある。

### 9.3 BlockFeatures と類似度

Embeddingは使わない。文字n-gram（初期は3-gramのset）を使い、text類似度はDiceで計算する。

```rust
pub struct BlockFeatures {
    pub exact_hash: ExactHash,
    pub ngrams: NGramSet,
    pub normalized_geometry: NormalizedRect,
    pub style: StyleFeatures,
    pub anchor_interval: Option<AnchorIntervalPosition>,
}
```

absolute page座標は改ページやreflowで大きく変わるため、positionは補助signalである。
利用する場合はpage内の正規化座標や前後anchor間の相対位置を使う。

### 9.4 Candidate Generation

全Block組を比較しない。candidate generationはtraitで切り離す。

```rust
pub trait CandidateGenerator {
    fn candidates(&self, old: &BlockFeatures, limit: usize) -> Vec<Candidate>;
}
```

候補集合は`exact/anchor候補 ∪ inverted-index候補 ∪ optional LSH候補 ∪ move候補`とする。
defaultはnew側`HashMap<NGram, Vec<BlockId>>`のn-gram inverted indexであり、共有n-gram数と希少性で候補を絞り、最終Dice scoreは候補に対して別途計算する。
MinHash LSHは長文書のoptional実装であり、holdoutを含むbenchmarkでcandidate recallを悪化させず、候補件数または実行時間を明確に改善した場合にだけdefault化する。

contentとabsolute positionを一つのhashへ強制しない。改ページ、margin変更、段落moveでpositionが変わるとtrue matchを候補から落とすためである。
LSH collisionや同一bucketであること自体はconfidenceへ加点せず、最終scoreとneighbor consistencyで検証する。
candidate生成の打ち切りを、対応する内容が存在しない証拠にしない。

### 9.5 Matching Score

重みは固定せず、利用するsignalだけを仕様とする。text類似、geometry類似、style類似、neighbor類似、anchor文脈である。
最も強いsignalはtextとし、geometryは補助にとどめる。
candidate generator由来の情報は候補へ入った理由として診断に残すが、それだけでmatching scoreを上げない。
数値マスク一致やgeometry一致だけによる確定も禁止する。

### 9.6 共通correspondence solver

supplierはtext、heading、typed identity、label、row/column名、neighbor、visual feature、embeddingなどを使ってよい。
ただしsupplierが単独で確定変更を出力することは認めない。
supplierは対応候補を提案し、共通solverがsource overlap、type互換、parent/neighbor整合、ownership、split/merge accountingを検証する。
footer detection等のdocument-family ruleも一つのsupplierであり、独立した確定権限ではない。

現在の目的関数は`ScopedIdentityThenLiteralThenPaddingThenInferredStructureV4`である。
source-backedなscoped identity、source-backedなliteral content、inferred structural correspondence、inferred literal content、その他のinferred proposalの順に、supplier weightをlexicographicに最大化する。
keyやlocal orderをmodelが供給した場合、その解釈はinferred classに留まり、解釈の正しさを証明しない。
等価なoptimal solutionが複数ある場合はambiguousのまま残し、探索が打ち切られたcomponentはmandatory correspondenceを出力しない。

solverはsource premiseだけでmandatoryなcorrespondenceを別に記録する。
source-backed proposalがinferred tie-breakerによって選ばれた場合、その結果自体をinferredとして報告する。
source-only保護はtie-breakerに依存せず、検証はcomponentのstate budgetを共有する。
component-size capを適用する前に、残る最高objective classのbounded searchでmandatory correspondenceを確定できる。その後、それと非両立な候補だけを除去できる。残りの探索は強制されたownershipとpartition制約を保持し、prefixとresidual stateは同じbudgetを共有する。prefixを完了できなかったことはrival除去の根拠にならない。

alternative correspondenceは明示的に保持する。探索打ち切りは、見かけ上唯一の候補を証明済みのunique matchへ昇格させない。
enumeration、validation、scoring、final checking、破棄したworkを課金する。
追加のview/recognition要求は競合時に限り、再訪はboundedで新しいdependencyを記録する。
correspondenceは局所exact claimを条件付きで支えるものであり、著者の編集履歴の証明ではない。

候補の所有は排他的だが、確定範囲は重複して所有しない。
複数の出現箇所を一つの変更にまとめる場合はそれぞれを判定し、未確定の出現箇所を候補へ分ける。

### 9.7 対応領域とその閉包

**対応領域**は、新旧間で対応を主張する内容の範囲と、その境界を組にしたものとする。
判定は次の情報を保持する。

| 情報 | 保持する内容 |
|---|---|
| 原文範囲 | old/newごとのblock、comparable token、canonical scalar範囲とglyph/pageへの逆写像 |
| 比較の前提 | 正規化policy、Unmappedのfont identity、合成space、採用した読み順と境界 |
| 対応領域 | 比較する内容、領域の境界、境界が改訂間で対応する根拠 |
| 探索状態 | 調べた競合候補、探索の打ち切り箇所、一意性を支える完全探索またはexact検証 |
| 判定 | 確定、候補、未解決の状態と、構造化した理由および根拠への参照 |
| 依存関係 | 判定が依存する抽出、境界、読み順、先行する対応。循環した根拠で互いを確定しない |

**領域が閉じている**とは、主張を変え得る内容や競合する対応を、その領域から根拠なく除外していないことを指す。
似た見出しが二つ見つかったことや、同じpage番号であることだけでは領域は閉じない。
文書の他の場所に対応がないことを根拠にする場合は、その場所まで探索するか未確定として残す。
小さな範囲の探索結果で、より広い範囲の一意性を主張しない。

対応領域は外側から内側へ構成する。
抽出が完全で単一の読み順を裏付けられる文書では、文書全体を最初の順序付き領域にできる。その読み順も比較modelの前提として記録する。
確定した親領域の中で、境界候補のexactな出現箇所を必要なtoken範囲全体で調べ、交差する割り当てや競合を除き、境界間の内容を欠落なく分割する。子領域は親の未解決な前提を引き継ぐ。
親や境界の対応を確定できない場合、recoveryは候補を提示できるが、領域が閉じたと見なして処理を進めない。

探索の完全性は、明示した対応modelの範囲で定義する。
modelには採用する構造上の順序制約、取り外せるsoft境界、区間を区切るexact anchor、繰り返し出現の扱いを含める。PDFのあらゆる解釈を列挙したという意味ではない。
類似度の足切りやtop-kだけで競合を定義から除き、確定を正当化することは認めない。
必要な探索を完了できなければ、その主張は未確定のまま残す。

### 9.8 構造推定が外れた場合の回復

glyph、line、block、region、trusted run、source mapを使い、構造化を可逆なviewとして扱う。
未解決の領域では、既存のblock view、原文が連続するline/run view、既存の境界に依存しないanchor recoveryの順に、有界かつ決定的に試す。
blockを分割または結合するviewは、tokenの所有範囲とseparatorの由来を保持する。
geometryは局所的な文字の相対値を使い、既知の文書名、producer名、page番号、特定の文言で処理を分岐しない。

回復の目的は、改行、改ページ、誤ったblock分割をまたいで正しい対応を増やすことである。
予算内で候補探索を広げ、他の領域が未解決でも独立した局所領域を確定できるようにする。
候補表示だけを改善して確定できる変更や範囲が増えない状態を完成とはしない。

### 9.9 Alignmentの段階

1:1、1:0、0:1に加え、同一anchor区間内で隣接Blockだけを結合する制約付き1:2 / 2:1を扱う。
これはCase 1/2で、line wrapやpage boundaryにより片方だけBlockが分割された場合に必要である。
1:3 / 3:1を含むgeneral DP alignmentへ拡張し、構造化誤差を吸収する。
Matchingは一回で確定せず、initial matching、neighbor consistency、refinementの順に進める。

## 10. Exact Diff とその先

### 10.1 Myers Diff

構造上有効な対応候補に対してMyersによるexact diffを実行する。
共通判定を満たす対応からは確定した変更を出力し、根拠が不足する対応から得た編集は原文を保持した差分候補として出力できる。
編集列がexactであっても、その前提となる対応付けの正しさまでは証明しない。

diffの入力はcanonical文字列、またはUnmapped領域では`(font_hash, glyph_id)`トークン列であり、matching文字列は使わない。
現在の実装は、bounded Myers、edit count bound、mandatory position、localized edit scriptを持つ。
literal claimは宣言したtokenと正規化のもとで全optimal insertion/deletion pathを量化する。
count bound、mandatory position、complete editing witnessは別の概念であり、位置が曖昧なpositive boundは変更の存在だけを示す。
complete monotone witnessにはequal residueが必要であり、minimalityとは独立である。

### 10.2 局所比較

whole-field / whole-cell操作は構造identityとold/new値を保持する。そのreview範囲は非所有のcontextである。
1件のdate置換が複数のexact character hunkを含み、間のequal characterを所有しなくてよい。
relationshipはtext/value multisetが同じでも変化し得る。移動やmembership変更を等価へ黙って還元しない。

visual regionは互換rendering profileのもとで比較する。
pixel差はprofile依存のvisual factであり、自動的なsemantic claimではない。
reflow、scale、antialiasing、producer効果と、image/figure内容の変更を分けて評価する。
未知のvisual meaningを変更された単語として主張しない。

formatting-only reportingはbest-effortであり、pixel-levelのrendering同一性を主張しない。

### 10.3 将来の拡張

- **Render Awareness**：render order、clipping、fill/stroke、CropBox、可視性まで扱う。現行実装はCropBoxと対応clipを分類し、非描画render modeを除外する段階である。透明・上塗り・複合clipの完全解釈は今後の課題である。
- **OCR統合（未実装）**：OCRは固有の画像参照とrecognition alternativeを持つevidenceとして扱い、native glyph IDを捏造しない。`SourceRef`は`native`、`native_vector`、`rendered`、`structured`の由来を排他的に区別し、認識された文字がglyphを偽装できない。将来の認識結果はrenderedまたはstructured evidenceと専用の由来へ結び付け、同じ可視内容のnative版とOCR版を等価または競合するviewとして扱う。比較は既存の証拠pipelineと局所比較を再利用するが、由来の偽装ではなく証拠の共有として行う。誤認識らしい差分（`0 ↔ O`、`1 ↔ I`など）も消さず、`OcrConfusion`タグとして報告する。現行実装はprovider供給の候補を保持できるが、認識の新規実行は行わない。
- **多段組と表**：XY-Cut、Region Graph、ruled grid、counterpart table、tagged structureによる部分実装から、一般的なreading orderとtable semanticsへ拡張する。

## 11. ロードマップ

Track A（PDF Backend + Primitive Extraction）とTrack B（Diff Engine）を並行させる。
Track Bは`Document<Glyph>` fixtureで進められるため、parser backendの選定やfont edge caseがAlignment開発を止めない。

旧段階の構成は履歴として維持し、現在の実装状況を併記する。
「実装済み」は現行コードとテストで確認できる段階、「一部」は主要部品が動いており残作業がある段階、「未実装」は残作業である。
ここに未実装と記した段階だけが残作業であり、実装済みの段階を開始前へ巻き戻すものではない。

### Track A：PDF Backend + Primitive Extraction

| Stage | 実装 | 確認 | 現在 |
|---|---|---|---|
| A0 | `PdfParser` / `ParsedPdf`中立境界、capability fixture、既存library比較、default backend選定 | `pdfdelta inspect doc.pdf --backend-info` | 実装済み（facadeと`lopdf` adapter） |
| A1 | 既存parser adapter、xref（table + stream）、ObjStm、trailer chain、Page Tree、Resources、stream decode、limit/error分類 | `pdfdelta inspect doc.pdf --objects` | 実装済み |
| A2 | Content Stream parser、Graphics/Text State、Form XObject、ToUnicode、Tj/TJ、Glyph座標、`/Rotate`正規化 | `pdfdelta inspect doc.pdf --glyphs` | 実装済み |
| A3 | Debug renderer：Glyph overlay SVG、canonicalページ座標、object/operator provenance | `pdfdelta inspect doc.pdf --svg debug.svg` | 実装済み |
| A4 | font decoding拡張（Type0、CID、Unmapped token）とDifferential Test | 抽出conformance suite | 一部実装（Type0/CID/Unmappedは対応済み、oracleはcurated 1件） |
| A5(optional) | alternate/custom `PdfParser` backend | §3.3のtriggerが再現し、同一suiteを通過 | 未着手（trigger未再現） |

A3のSVG上で、PDFに実際に見える文字と復元したGlyphが正しい座標で重なり、各GlyphからContent Stream/operatorへ戻れる状態をTrack Aの最初の観測可能なゴールとする。
A5は既定のマイルストーンではない。

### Track B：Diff Engine

| Stage | 実装 | 完成条件 | 現在 |
|---|---|---|---|
| B0 | Workspace骨格、GlyphExtractor境界、Glyph document fixture、error types、parser-backed composition | `pdfdelta --help`とGlyph fixture test | 実装済み |
| B1 | Glyph → Line（space再構成含む） | 1段組、多フォントサイズ、上付き、日英のbenchmark | 実装済み |
| B2 | Line → Block | 段落、見出し、改ページ、spacing差のbenchmark | 実装済み |
| B3 | canonical正規化、exact anchor、CandidateGenerator trait、n-gram inverted index、1:1 + 制約付き1:2/2:1 alignment、insert/delete、Myers Diff、Changeスキーマ、exit code | §2.2の5ケース（最初の実用版） | 実装済み（両rendererで5ケース通過） |
| B4 | 1:3/3:1を含むgeneral DP alignment、neighbor consistency、move検出 | ParagraphMoveと複雑なBlock split/mergeを含むbenchmark | 実装済み（制約付き1:N/N:1、neighbor consistency、move検出） |
| B5(optional) | MinHash LSH candidate generator、large-document profiling | §9.4を満たす場合だけdefault候補 | 一部実装（LSH実装済み、defaultはinverted index） |

B4が最も重要な技術的マイルストーンである。
B5はcorrectness機能ではなくscalability改善であり、B3/B4を先に成立させる。

### 後続Stage（Track合流後）

| Stage | 内容 | 現在 |
|---|---|---|
| C1 | Multi-column：XY-Cut、Region Graph、UNKNOWN fallback | 一部実装（XY-CutとRegion Graphは実装、一般的な段組の読み順は未解決のまま） |
| C2 | Parallel / Table / Form：新旧対照表、vector line、grid推定、label/value、AcroForm値 | 一部実装（ruled grid、form値、counterpart tableは実装、一般の表と新旧対照表は未完） |
| C3 | Render Awareness：z-order、clipping、可視性、CropBox | 一部実装（render mode、CropBox、対応clipを分類。透明・上塗り・可視性は未完） |
| C4 | OCR統合 | 未実装（§10.3の証拠契約に沿うprovider待ち） |

現在の優先課題は、可視native文字を両側に持つ改訂ペアで比較完了を増やすことである。
空白・改行・繰り返し文字が残す対応の曖昧さを元の文字と位置の証拠から解消し、重複検証を減らして既定の探索上限内で比較を完了させる。
フォントや抽出の不足は、現在の対象PDFで再現する問題から対応する。
受入条件や探索上限を緩めて完了数を増やしてはならない。

A4のconformance oracle拡充とC1〜C4の未完部分は長期的な課題である。
一般的な表の解釈、画像内文字の認識、OCR providerの実装は、現在のnative文字比較を改善する作業の前提にはしない。

### 証拠チャネル移行の実装順序と現在

1. 証拠storeとchannel選択。— 実装済み
2. document graph adapterとprovider（native text、ruled table、tagged structure、form、image）。— 一部実装
3. 共通correspondence solverとsupplier。— 実装済み（objective V4）
4. typed comparisonと公開出力（text report、channel JSON、review bundle）。— 一部実装（text/forms/image/relationの局所比較とchannel report schema 2）
5. 資源制御と汎化評価。— 一部実装（limitsとfuzzingは実装済み、汎化評価は継続中）

glyph adapterは移行中も維持する。
document-family ruleの追加やconstructed fixtureだけでgraphを動かすことは、この移行の完了を意味しない。

### 最初に実装しないもの

OCRの新規実行、LLM、Embedding、semantic similarity model、visual pixel diffの完全化、高度な表認識、AcroForm全般、annotation diff、full PDF 2.0、GPU、WASM、Web UI、性能最適化、PDF object parserの自作。
これらは現行incrementの対象外であり、恒久的な禁止ではない（§2.1のscopeと§10.3の将来課題を参照）。

## 12. テストと検証

### 12.1 Benchmark Generator (pdfdelta-bench)

Canonical Document Spec（YAML）からPDFを生成する。

```yaml
document:
  title: Quarterly Service Report
  sections:
    - id: availability
      heading: Service availability
      paragraphs:
        - id: availability-p1
          text: Release 10 remains available during the transition.
```

生成には複数系統のrendererを使う。単一writerのPDFだけでテストすると、そのwriter固有の内部構造へoverfitするためである。
現在のmatrixはproject-ownedの`lopdf-tj`と`classic-xref-tj`の2経路、24ケース、48 recordで構成する。
外部renderer（Typst、Tectonic）のPDFはvendored fixtureとして保持し、renderer本体をCIで実行しない。

### 12.2 Mutation Engine

Canonical Documentへ二種類のmutationを適用する。

- **Semantic Mutation**（正解diffを発生させる）：TextReplace、TextInsert、TextDelete、NumberReplace、ParagraphInsert、ParagraphDelete、ParagraphMove。
- **Rendering Mutation**（content diffを発生させない）：FontSizeChange、MarginChange、PageSizeChange、LineHeightChange、LineBreakChange、PageBreakChange。ColumnChangeはmulti-column benchmarkで有効化する。

これにより「見た目は大幅変更 + `Release 10` → `Release 20` だけ内容変更」のようなケースを自動生成する。
evaluatorは、報告されたChangeと期待Changeをkind一致とspan overlapで照合する。

### 12.3 5つの初期受入条件

`pdfbench verify`は、改行位置だけ違う、改ページ位置だけ違う、1件のtext replacement、1件のparagraph insertion、1件のparagraph deletionを、両renderer経路で検証する。

現在の結果は次の通りである。

- 5つの受入条件を名指しで含む48/48 recordが通過し、exit code 0を返す。
- 生成matrix全体では42 recordがstrictに合格し、6 recordはedit境界の曖昧さのためcandidate-policy matchとして保持される。candidateはexact acceptanceに数えない。
- 5ケースの期待semantic eventとchanged tokenは、両rendererでprecision/recall 1.0である。

### 12.4 実データベンチマーク

合成データだけではpdfdelta-bench固有の癖にoverfitする。
公開されている規程、policy、report、manualなどの改訂ペアを、利用条件を確認した上でsourceとして使う。
公開説明がexact spanや全変更を機械可読で与えるとは仮定せず、採用pairごとに人手でreviewしたexpected manifestを作る。

現在の固定panelは36ペアであり、文書系列とproducer系列を一つのsplitへまとめる。
調整には開発用グループを使い、評価用グループは調整前に固定する。
native baselineはpanel全体を対象に取得し、最終binaryの完了確認は少なくとも3/36ペア（各2回）である。
36ペア全体の最終binaryでの再測定は行っていない。
holdoutは新規に凍結した未使用pairだけを使い、panelの分母やscopeを変更しない。

完全なannotationから求めるprecisionとrecallはannotationの範囲に限る。
部分annotationは列挙した変更のrecallを評価できるが、precisionを主張する分母には使わない。
品質評価を省略したpairや抽出に失敗したpairも実行結果の集計に残す。

### 12.5 Differential Testing / Backend Conformance

**Parser backend conformance**：同一fixtureをdefault backendとalternate parserへ入力し、pdfdelta中立型へ正規化した上で、page順、reference解決、resource inheritance、decoded Content Stream bytes、object stream内object、incremental updateのlatest objectを比較する。library固有のdebug文字列やobject配置順は比較しない。

**Primitive Extraction differential**：同一PDFに対し、pdfdeltaの`ParserBackedGlyphSource`と独立したposition-aware extractor / interpreterを比較する。
decoded text、Glyph数、text order、bbox、baselineを許容誤差付きで比較し、SVG overlayで人間が確認できるようにする。
plain textしか返さないextractorはtextの補助oracleとして使い、geometryの正解とはみなさない。
同じunderlying parserを共有するtool同士だけの一致は独立oracleとみなさず、hand-written fixtureまたは別実装で裏付ける。
curated oracleは`fixtures/extraction-conformance/pdf-oxide-0.3.77/`にあり、vendored Japanese Typst fixtureの158 mapped horizontal glyphを独立parserと照合する。unmapped、rotated、corpus横断のoracleは今後の課題である。

### 12.6 Property Testing / Fuzzing

`proptest`をtest用依存として使い、次を検証する。

```text
fixture object → backend adapter → equivalent neutral object
normalize(normalize(x)) == normalize(x)
diff(x, x) == empty
alignment(x, x) == identity
candidate_generator(x) contains identity match
```

PDFはuntrusted inputとして扱う。parser入口、中立object変換、CMap parser、Content Stream parser、FontDecoder、glyph抽出、layout、native証拠、graph、text比較をfuzz targetとする。
malformed PDFでpanic、無限ループ、無制限のメモリ確保を起こさない。
default limitは有限かつ設定可能な状態を維持し、cargo-fuzzはnightlyを要するためfuzz crateだけstable制約の例外とする。
有限時間のfuzz実行から、crashしないこと全般を証明したとは扱わない。

### 12.7 Candidate Generation評価

小規模fixtureではexhaustive all-pairs scoreをoracleとして保存する。
合成fixtureのcanonical paragraph idとspan overlapからtrue counterpartを定義し、各candidate generatorについて次を測る。

- true matchがtop-K候補に含まれる割合（candidate recall）。
- old Blockあたりのcandidate数（p50 / p95 / max）。
- index build時間、query時間、memory。
- ParagraphMove、改ページ、margin変更時にposition featureがrecallを落としていないこと。

MinHash LSHのband数、signature長、K等は合成benchmarkで調整し、人手review済みのreal-world holdoutは評価にのみ使う。
LSH導入前後で最終Changeの正解率が変わった場合、candidate recall低下をbugとして扱いdefault化しない。

### 12.8 品質ゲート

implementation commit前に次を実行する。

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --all-features --lib
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --document-private-items
cargo run -p pdfdelta-bench -- verify
```

benchmark matrixまたは抽出適合性を変更した場合は、該当する`cargo test -p pdfdelta-bench --test bench_matrix`または`cargo test -p pdfdelta-bench --test extraction_conformance`で対象の条件を確認する。
実文書の評価には、実行command、manifest、annotation hash、source revision、options、raw resultを残す。
外部入力を取得できなかった実行は、合格ではなく実行不能として記録する。
単一runのtimingは観測であり、end-to-endの速度主張ではない。

## 13. 依存候補・参考実装一覧

production dependencyは現行のCargo.toml / Cargo.lockを根拠とする。

| 対象 | 現在の採用 | 主に見るもの |
|---|---|---|
| PDF object parser | `lopdf` 0.45.0（default features無効） | object、xref、object stream、incremental update、raw stream access |
| 描画 | `hayro` 0.7.1（CLI）、`hayro-cmap` 0.1.0（core） | visual channel、CID CMap参照data |
| CLI | `clap` 4、`clap_complete` 4 | 引数解析、補完生成 |
| 直列化 | `serde` 1、`serde_json` 1（float_roundtrip）、`serde-saphyr` 1 | JSON、YAML fixture |
| Unicode | `unicode-normalization` 0.1、`unicode-segmentation` 1、`unicode-bidi` 0.3 | NFC、grapheme/word、bidi安全化 |
| その他 | `flate2` 1.1、`sha2` 0.11、`smallvec` 1、`rayon` 1、`base64` 0.22、`png` 0.18、`rustix` 1（Linux/macOS） | stream decode、hash、buffer、並列、worker |
| dev / test | `proptest` 1、外部oracle、外部renderer | property test、conformance、fixture生成 |

参考実装として、`pdf-extract`をtext oracle、`pdf-inspector`をposition-aware抽出の参考、`printpdf`をToUnicode/Type0/simple fontの参考、`similar`と`imara-diff`をMyers edit scriptの妥当性比較、`pdf-writer`をlow-level fixture生成、Typst/Tectonic/krillaを別系統rendererとして参照する。
比較対象の抽象度を混ぜず、library固有型を上位層へ漏らさない。
parser backendの最終選択はcapability fixtureで決める。

## 14. 開発判断の原則

新機能は、「これがないと現在のBenchmarkで何が失敗するのか」を説明できる場合にだけ追加する。

妥当な例：

- Block segmentationミスが主要なFalse Positive → 1:N alignmentを追加。
- 1万Block文書でinverted indexのcandidate数が膨張し、exhaustive oracle比のrecallを維持したままLSHで改善できる → MinHash LSHを追加。
- 既存parser backendが必須fixtureのObjStmを誤解釈し、alternate backendやadapter修正でも解決できない → custom `PdfParser` backendを追加。

行わない例：

- 将来役立ちそうという理由でLLM embeddingを追加。
- PDF parserはいずれ必要そうという理由で最初からobject parserを自作。
- correctness検証なしにLSHを最終matchingへ使用。
- 一つの自然文書を通すために、その文書固有の条件分岐やlimit緩和を追加。

常に次の順で進める。

```text
実PDFで失敗を確認
→ 原因の層を分類（parser / extraction / layout / candidate / alignment / diff / correspondence）
→ 最小限の機能またはbackend差し替えを追加
→ exhaustive oracleとBenchmarkで改善確認
```

証拠の保持、可逆な構造化、明示的な未解決報告を、短期的な検出率より優先する。
document-family ruleは複数の独立した証拠で裏付けられるまで候補に留める。

## 15. 仕様変更履歴

この節は、仕様を変更した理由と変更箇所を`SPEC.md`自身に残すための記録である。
過去分は`git log --follow -- SPEC.md`と各commitのdiffから復元した。詳細は`git show <commit> -- SPEC.md`で確認する。

### 2026-09-20 日本語15章構成の復元と現行仕様への更新（本変更）

- v5英語版（`3586485`）を置き換え、v3/v4の日本語15章構成を復元した。
- v4の§16「未知のPDFに対する比較設計」の内容を、§2の完了条件、§5の出力契約、§9の対応領域と回復、§12の評価へ統合した。
- v5固有の証拠チャネル、共通solver、局所失敗、安全境界を、§2、§3、§4、§9、§10、§12へ統合した。
- 主評価を可視PDF-native文字とし、画像、パス文字、OCRの新規実行は対象外、既存の可視OCR文字層とForm内native文字は対象、非描画textは対象外と明記した。
- 5つの初期受入条件は変更していない。
- 依存バージョンを現行Cargo.toml / Cargo.lockに合わせて整理した。

### 2026-09-08 未知PDFに対する比較設計（v4）

- §5：確定した差分と要確認の候補を分離し、候補と位置未確定の領域を解決済みcoverageから除外する仕様を定めた。不完全な比較を既定でexit 3にする移行方針も追加した。
- §9と§10：候補生成、共通の対応付け判定、exact diffの責務を分け、scoreやexactな編集列だけでは対応を確定しないことを明記した。
- §11と§12：既存の受入条件を維持し、文書系列とproducer系列の混入を防ぐ評価方針を追加した。
- §16（現行版では各章へ統合）：対応領域の条件、可逆な構造回復、範囲の所有関係、資源制限、結果の型、互換性の移行、検証matrix、公開条件、実装順序を記載した。

### 2026-08-24 人間向けreportのunified diff化

- §5.3：text reportを、exact diff結果の表示層のみの投影として文脈付きunified diff形式へ刷新した。要約行、`---` / `+++`、1-based page付きhunk header、`-` / `+`行、有界なcontext、`~ moved`、`?`による未解決表示、近接exact changeの表示上の統合、`--color auto|always|never`を定義した。`Comparison`とJSON reportの意味は不変とした。

### 2026-08-23 backend依存のupstream復帰

- §6.2：`lopdf`依存をupstream `J-F-Liu/lopdf`のmain revisionへ戻し、xref-stream entry数のdecoded body上限、object streamの非破壊parse、非標準`/BrotliDecode` filter、stream `/Length`不一致の回復、startxref解決失敗時の有界なxref再構築fallbackを取り込んだ。現在はcrates.ioの0.45.0をpinしている。
- §6.2：公開test corpus由来の実PDFをstrict自己比較へ投入し、対応待ちだったBrotli prototypeとLength 0 XObjectをstrict完走へ移行した。

### 2026-08-21 追加コーパスと入力付き対応

- §2.1、§3.2、§5.4、§6.2：借用password APIとside別password file入力を追加し、password本文をargv、report、traceへ残さない規則を定義した。
- §6.2：Page Treeの正常枝を保持し、壊れた枝とroot Count不一致をdocument-scoped UNRESOLVEDとして部分成功へ変換する規則を追加した。
- §6.4：未埋め込みCID fontへのside別external identity assertion、named Resourcesへ依存するType 3 identityの上限付きcanonical graph、custom Type0 CMapのfull-domain identity限定、CID FontDescriptorのFontBBox fallback、Standard 14のcanonical identity拡張を追加した。
- §6.3：Form XObjectのstate分離、`/Contents` arrayをまたぐoperand回復、大小反転page boxの正規化を明文化した。
- §12.6：公開PDF corpusの実測に基づく有限なoperand node、diff token、3-gram elementのdefault上限と、明示的な低上限を維持する原則を追加した。

### 過去の変更

| 日付 | Commit | 変更した仕様 |
|---|---|---|
| 2026-09-10 | `3586485` | 英語v5へ全面置換。証拠チャネル、共通solver、局所失敗、安全境界を定義した。本版で日本語15章構成へ復元・統合した。 |
| 2026-08-21 | `111ffd5` | §2.1にempty user passwordで復号できるPDFを追加し、§6.2にpassword、security handler、秘密情報の境界を定義した。§6.4にFontBBox fallback、初期Type 3 subset、Identity-V subsetを追加した。 |
| 2026-08-20 | `cf1156e` | canonical / matching規則、soft line break、normalization eventを自前実装の責務として明確化し、Unicode crateへ委ねる範囲をNFCとsegmentation primitiveに限定した。 |
| 2026-08-20 | `c95378d` | §1と§2.2の例を一般的なrelease変更へ差し替え、§5.1に複数Blockのseparatorを含む`TextSpan`規則を追加した。§8と§12では数値mask例、benchmark fixture、公開real-world pair、holdout運用を一般化した。 |
| 2026-08-20 | `1db8dd1` | §2.2の受入条件3を`Release 10`から`Release 20`への1 replacementとして明文化した。 |
| 2026-08-19 | `2324f4c` | 初版SPECを追加し、目的、scope、architecture、data model、pipeline、roadmap、benchmark、resource limitの基本方針を定義した。 |
