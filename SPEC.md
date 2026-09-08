# pdfdelta 技術仕様書 v4

2026-09-08改訂：未知のPDFに対して、根拠に基づいて比較結果を確定する設計を追加した。
差分候補の分離、共通の対応付け判定、coverageの再定義、不完全な比較に対する既定の終了コード変更は、これから実装する仕様である。
現行実装がこれらを満たしていることを示すものではない。

## 1. 目的

二つのPDFを比較し、文章として意味のある変更だけを検出するCLIツールをRustで実装する。

```bash
pdfdelta old.pdf new.pdf
```

旧版の `Release 10 remains available.` が新版で `Release 20 remains available.` になっていれば、これを1件の置換として検出する。
一方、文章内容が変わらないまま改行位置、改ページ位置、フォントサイズ、余白、段組、生成ソフトウェアだけが変わった場合は、変更に数えない。

検出対象は、PDF内部表現の差ではなく、人間が文書として認識する内容の差である。

### 1.1 設計の骨格

全体は次の思想で組む。

```text
Raw PDF Evidence → Imperfect Structure → Robust Alignment → Exact Diff
```

- 構造化(Line/Block復元)に100%の精度を要求しない。構造化の誤りはAlignmentで救う。
- Alignmentでは多少の文字変更を許容して対応付ける。
- 最後のExact Diffだけは変更を一切ごまかさない。`Release 10` → `Release 20` のような小さな変更を絶対に消さない。
- どの段階でも、確信が持てない場合は誤った結果を返さず UNRESOLVED として報告する。

### 1.2 普通のdiffが使えない理由

PDFは論理的な文章構造を保存する形式ではなく、「この文字をこのフォントでこの座標へこの順番で描画する」という命令の列に近い。
そのため plain text 抽出 → diff という構成は次の点で壊れる。

- **改行**：同じ文が2行に分かれているだけで差分になる。
- **改ページ**：同じ段落がページ境界を跨いで移動しただけで差分になる。
- **段組**：左右2列の読み順は文書種別で異なる。論文なら左列→右列だが、法律の新旧対照表なら左右が行単位で対応する。「左右に分かれている」という情報だけから読み順を決めてはいけない。
- **構造化の揺れ**：同じ文章でも、版によってBlock分割が1個になったり2個になったりする。完璧な文書構造を前提にした設計は成立しない。

---

## 2. スコープと完成条件

### 2.1 最初の対象

born-digital PDFに限定する。

| 扱う | 扱わない(初期) |
|---|---|
| 日本語・英語(初期完成条件は横書き) | 縦書きの完全対応、OCR、スキャンPDF、手書き |
| 1段組の複数ページ文書 | AcroForm、高度なannotation |
| 基本的なフォント(ToUnicodeあり/なし両方。§6.4参照) | 複雑な表、画像内容比較 |
| xref stream / object stream を含む現代的なPDF | PDF 2.0固有機能の網羅 |
| empty user passwordまたは明示passwordで復号できる暗号化PDF | password不明の暗号化PDF、未対応security handler |

非対応機能に遭遇した場合は、誤った結果を返すのではなく UNSUPPORTED / UNRESOLVED として扱う。

補足：xref streamとobject stream(PDF 1.5)は「扱う」側に入れる。現代のborn-digital PDFはこれらがデフォルトであり、classic xref tableのみの対応では最近のPDFがほぼ読めないためである。

これはpdfdeltaがobject parserを自作するという意味ではなく、初期の既存`PdfParser` backendが満たす受入条件である。

### 2.2 最初の完成条件

次の5ケースを確実に解いた時点を最初の実用版とする。

| Case | 内容 | 期待値 |
|---|---|---|
| 1 | 改行位置だけ違う | 0 changes |
| 2 | 改ページ位置だけ違う | 0 changes |
| 3 | Release 10 → Release 20 | 1 replacement |
| 4 | 段落追加 | 1 insertion |
| 5 | 段落削除 | 1 deletion |

Case 1は英語文書も含む。英語PDFはspace glyphを描画せず座標移動でspaceを表現することが多いため、space再構成(§7.1)がこの完成条件の前提になる。

二段組と表はこの時点の完成条件に含めない。

---

## 3. アーキテクチャ

### 3.1 パイプライン

```text
PDF bytes
 │
 ▼ PDF Parser Backend       … object / xref / page / stream access
 │
 ▼ Primitive Extraction     … Content Stream interpreter → Glyph + geometry
 │
 ▼ Layout Reconstruction    … Line / Block / Region
 │
 ▼ Cross-document Alignment … old ↔ new の対応付け
 │
 ▼ Exact Diff               … Myers diff
 │
 ▼ Changes / Confidence / Coverage / Unresolved
```

PDF構文・object graphの解決と、描画命令からGlyphを復元する処理を分離する。既存parser libraryを使っても、lossyなplain text extractorへ依存する必要はない。後段が必要とするraw object、Content Stream、Resources、object idはparser境界越しに保持する。

各段は独立させる。Layout Reconstructionが間違ってもAlignmentで救い、Alignmentが不確実ならUNRESOLVEDに落とす。

### 3.2 PdfParser境界とGlyphExtractor境界

PDF object parsing専用のtraitと、Glyph抽出専用のtraitを別々に置く。

```rust
use std::sync::Arc;

pub trait PdfParser: Send + Sync {
    fn parse(
        &self,
        pdf: Arc<[u8]>,
        limits: ParseLimits,
    ) -> Result<Box<dyn ParsedPdf>>;

    fn parse_with_password(
        &self,
        pdf: Arc<[u8]>,
        limits: ParseLimits,
        password: &str,
    ) -> Result<Box<dyn ParsedPdf>>;
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
    fn extract(
        &self,
        pdf: &dyn ParsedPdf,
        limits: ExtractionLimits,
    ) -> Result<Document<Glyph>>;
}
```

`PdfVersion`、`ObjectRef`、`PdfObject`、`PdfDict`、`RawStream`、`DecodedStream`、`ParseLimits`、`ExtractionLimits`はpdfdelta側の中立型とし、`lopdf::Object`のような特定libraryの型を後段へ漏らさない。`ParsedPdf`は完全なPDF DOMを再設計するための境界ではなく、Primitive Extractionに必要な最小capabilityを安定化するためのfacadeである。

初期default backendは、既存のPDF parser libraryをadapter越しに利用する。第一候補は`lopdf`とし、A0のcapability spikeで必須fixtureを通らない場合は`pdf-rs`等の別adapterへ差し替える。上位層はこの選択を知らない。

production pipelineでは、`PdfParser`とpdfdelta独自の`GlyphExtractor`を`ParserBackedGlyphSource` facadeで合成し、PDF bytesから`Document<Glyph>`までを提供する。Track Bの単体開発は、PDFを経由せずserialize済みまたはprogrammatically constructedな`Document<Glyph>` fixtureを直接入力して進める。既存text extractorは主backendにせず、Differential Testingのoracleとしてのみ使う。

### 3.3 既存parserを先に使う方針と自作backendの条件

初期実装ではPDF object parserを自作しない。Header、lexer、indirect object、xref、object stream、trailer、Page Tree、stream decodeは既存libraryへ委譲し、pdfdeltaの独自実装は「差分に必要な証拠を失わずGlyphへ復元する処理」から始める。

自前のContent Stream / Text State interpreterは必要である。既存のplain text extractorはrender order、文字単位のgeometry、raw code、Resourcesの由来を捨てることがあり、Exact Diffまで追跡可能な証拠として不足するためである。ただし、これはPDF object parserまで自作する理由にはならない。

自作PdfParser backendは、次のいずれかをbenchmarkまたはfixtureで確認した場合にだけ追加する。

- 必須scopeのxref stream、ObjStm、incremental update、resource inheritanceを既存backendが正しく扱えず、adapterやupstream修正で解決できない。
- Content Streamのraw bytes、object identity、filter情報など、後段に必要な証拠を取得できない。
- malformed PDFへのresource limitやerror分類を既存backend上で安全に実装できない。
- Differential Testingで既存backendの誤りが再現し、代替backendでも要件を満たせない。

自作backendを追加しても、`PdfParser` / `ParsedPdf`境界と上位pipelineは変更しない。自作backendへの切り替え自体をロードマップ上の既定ゴールにはしない。

### 3.4 Workspace構成

```text
pdfdelta/
├ Cargo.toml
├ crates/
│  ├ pdfdelta-core/        # pure library。CLIを知らない
│  │  └ src/
│  │     ├ pdf/
│  │     │  ├ backend/    # PdfParser / ParsedPdf + library adapters
│  │     │  ├ object/     # pdfdelta中立型
│  │     │  ├ content/    # Content Stream / Text State interpreter
│  │     │  └ font/       # font decoding (§6.4)
│  │     ├ source/        # ParserBackedGlyphSource / Glyph fixtures
│  │     ├ model/
│  │     ├ layout/
│  │     ├ normalize/
│  │     ├ alignment/
│  │     │  └ candidate/  # inverted index / optional MinHash LSH
│  │     ├ diff/
│  │     └ report/
│  ├ pdfdelta-cli/        # 入出力のみ
│  └ pdfdelta-bench/      # テストPDF生成と評価
│     └ src/
│        ├ document/ mutation/ renderer/ evaluator/
├ fixtures/
├ benchmark/
│  ├ manifests/ expected/ generated/
│  └ realworld/           # 人手review済みの公開改訂ペア (§12.3)
└ fuzz/                   # このcrateのみnightly許容 (§12.6)
```

crate名とバイナリ名は次の対応とする。

| crate | 種別 | バイナリ |
|---|---|---|
| `pdfdelta-core` | lib | — |
| `pdfdelta-cli` | bin | `pdfdelta` |
| `pdfdelta-bench` | bin | `pdfbench` |

`pdfdelta` は暫定名である。確定前にcrates.io、GitHub、npmで再確認する。変更する場合、影響はcrate名、バイナリ名、リポジトリ名に閉じており、本仕様の設計内容には及ばない。

### 3.5 言語と依存の方針

stable Rust、Rust 2024 Editionを前提とする(例外はfuzz crateのみ)。

**自前実装するもの**：parser library adapterと中立facade、Content Stream / Text State解釈、ToUnicode/CMapの必要部分、Glyph geometry復元、Glyph→Line→Block→Regionの構造化、§8.1と§8.2のcanonical/matching規則、soft line break policy、normalization event記録、Anchor検出、candidate generation、Alignment(1:N/N:1、move検出、confidence/coverage算出)、Myers Diff。

**production dependencyとして使ってよいもの**：A0で選定した既存PDF object parser backend(初期候補`lopdf`)、CLI parsing(`clap`)、JSON(`serde`/`serde_json`)、logging、error整形、必要なstream decode、decode済みtextに対するNFCとgrapheme/word segmentationを提供するUnicode character-data crate(`unicode-normalization`、`unicode-segmentation`)。Unicode crateへ委ねるのは文字dataとこれらのprimitiveに限り、§8.1と§8.2の規則、soft line break policy、normalization event記録は自前実装に保つ。圧縮アルゴリズムや汎用PDF object parserの再実装は初期目的から外れる。

**dev / test dependency**：非選定のparser候補(`pdf-rs`等)、`pdf-extract`、`pdf-inspector`、property-based testing(`proptest`)、benchmarking。特定のextractorが`GlyphExtractor`契約に十分なgeometryとprovenanceを返せる場合に限り、実験実装として接続してよいが、production設計をその出力形式へ合わせない。

---

## 4. データモデル

### 4.1 Glyph

```rust
pub struct Glyph {
    pub id: GlyphId,
    pub text: DecodedText,      // §6.4
    pub raw_code: Vec<u8>,
    pub page: PageId,
    pub bbox: Rect,             // 座標はf64
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

座標は/Rotateを適用済みのcanonicalページ座標系で格納する。ページ回転の正規化を後段に持ち込まない。

`bbox`は初期段階ではexactなink outlineではなく、font width・ascent/descentと変換行列から求めるlayout bounding boxとする。rotated textでは`baseline`と`direction`も併用し、axis-aligned `Rect`だけで順序を決めない。outline由来のink bboxが必要になった場合は別fieldとして追加する。

`render_mode`と`provenance`は、invisible textや上書き描画を後から検証し、parser backend間の差をobject/operator単位まで追跡するために初期から保持する。初期stageで完全なvisibility判定を行わなくても、証拠を捨てない。

初期段階ではperformanceより観察可能性を優先する。`String`や`Vec`を多少多く使ってよく、arena、interning等は構造が安定してから検討する。

### 4.2 可逆な構造化

構造化は不可逆変換にしない。

```rust
pub struct Line  { pub glyphs: Vec<GlyphId>, /* … */ }
pub struct Block { pub lines: Vec<LineId>,  /* … */ }
```

Block分割を間違えた場合に、元のGlyph列へ戻って再解析できることが、Alignment側での救済(§9.6)の前提になる。

---

## 5. Change スキーマと出力

diffの粒度と出力形式は、evaluatorやCLIより先に確定させる必要があるため、ここで定義する。

### 5.1 Change

`Comparison.changes`には、位置と種類を確定した変更だけを格納する。
推測を含む編集は、原文範囲、競合候補のグループID、未確定の理由、必要に応じた順位を持つ`change_candidates`へ分離する。
`Confidence`の値だけでは変更を確定しない。
変更の存在は確認できるが位置を特定できない領域は、`proven_changed_regions`として別に保持する。
以下の変更データは、§16の共通判定を通過してから確定結果へ含める。

```rust
pub enum ChangeKind { Replacement, Insertion, Deletion, Move }

pub struct Change {
    pub kind: ChangeKind,
    pub old_span: Option<TextSpan>,   // Block集合 + canonical文字範囲
    pub new_span: Option<TextSpan>,
    pub confidence: Confidence,
    pub tags: Vec<ChangeTag>,         // 例: CharacterWidth, OcrConfusion
}
```

`TextSpan`はBlock集合、複数Blockを結合した際の`BlockSeparator`、canonical文字範囲、comparable token範囲を保持する。A single-block span has no separator. Multi-block spans require `Concatenate`, `Space`, or an explicit per-boundary pattern. The current ordered alignment supports at most three blocks per group; mixed patterns therefore store two boundary decisions.これにより同じBlock集合でも結合方法によって変わる範囲indexを一意に解釈できる。

変更の単位は「対応付いたBlock集合の上のcanonical文字範囲」とする。範囲indexは§8のUnicode scalar indexであり、`MappedText.source_map`を通じてGlyph、page、geometryへ逆写像できる。
「1 replacement」とは、連続するcanonical文字範囲の置換1件を指す。

### 5.2 Confidence と Coverage

confidenceは判断の補助情報であり、正解確率や正しさの証明として扱わない。
各側の抽出済みcomparable tokenを、同一と確定した範囲、変更と確定した範囲、未解決の範囲に重複なく分け、最初の二つだけを解決済みcoverageへ含める。
候補がある範囲と、変更位置を特定できない範囲は未解決のまま残す。
この規則は「変更なし」の判定にも適用する。
抽出の完全性と、画像など比較対象外の内容は、token比率と分けて報告する。

結果には必ず、個々の判定の確からしさと、比較できた範囲を分けて持たせる。

- **Confidence**：対応候補を評価する補助情報。正解確率や単独の確定条件にはしない。
- **Extraction completeness**：全pageのContent Streamと、そこから到達するtext-bearing Form XObjectをparser backendとPrimitive Extractionが処理できたか。画像XObjectはscope外として明示的にskipできるが、種別不明または未解釈のstreamを「文字数0」としてcoverageの分母から消してはいけない。
- **Old alignment coverage**：old側で共通判定を通過した同一範囲と変更範囲のcomparable token数 / old側で抽出できたcomparable token総数。
- **New alignment coverage**：new側で共通判定を通過した同一範囲と変更範囲のcomparable token数 / new側で抽出できたcomparable token総数。

comparable tokenにはcanonical Unicode scalarとUnmapped tokenを含む。
確定した1:0 deletionと0:1 insertionは、内容が存在する側の解決済みcoverageへ含める。
変更が多い文書でも、変更箇所を特定できていれば比較は解決しているためである。
CLIではold/newの抽出済みtokenに対する比率の最小値を要約として表示できるが、JSONには両側を別々に保持する。
抽出が不完全な場合は`extraction_complete: false`と問題の範囲を併記し、量が不明な欠落を0 tokenとして扱わない。
分母が0の場合は百分率を表示せず、抽出状態と対象範囲から完全性を判断する。
ページ面積比は表示上の参考値にとどめる。

### 5.3 出力カテゴリ

出力は、位置まで確定した変更、位置が未確定の変更領域、差分候補、Formatting-only、未解決領域に分ける。
差分候補には`TENTATIVE`を明示し、未確定の理由と原文の位置を表示する。
候補の件数は確定した変更件数に含めず、候補の範囲も解決済みcoverageへ加算しない。
この分離は次のJSON schema versionで導入し、現行のversion 9の意味を黙って変更しない。

canonical正規化(§8)で吸収した差も黙って消さず、独立したカテゴリで報告する。canonical textが同一のaligned spanについて、line/page break、Block分割、font size、position等の差を確実に識別できた場合もFormatting-onlyへ含める。このカテゴリはbest-effortであり、0件でもrenderingが完全同一であることは保証しない。exit codeのContent change判定には影響させない。

人間向けtext reportは、exact diffの結果をreviewしやすい文脈付きunified diff形式へ投影する。これは表示層のみの変換であり、`Comparison`の変更列やJSON report(§5.2)の機械可読な意味は一切変わらない。要約行に全出力カテゴリとcoverageを1行で併記し、`---` / `+++`のfile header、`@@ page N … @@`(page番号は1-based)のhunk header、`-` / `+`の隣接行、変更箇所周辺の有界なcontext、移動は`~ moved`明示、未解決領域は`?`行で可視化する。近接するexact change同士(同一Block集合かつ一定以下のequal tokenで分離)は表示上1つのhunkへ統合する。ANSI色は`--color auto|always|never`で制御し、既定の`auto`はstdoutがterminalの時だけ着色する。色は`-` / `+`記号の補助であり、色なしでも出力は読める。

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

「No differences found」とだけ表示することはない。

### 5.4 CLI と exit code

初期CLIは次の三つとする。CLI parserには`clap`を使う。

```bash
pdfdelta inspect document.pdf
pdfdelta inspect document.pdf --backend-info
pdfdelta inspect document.pdf --objects
pdfdelta inspect document.pdf --glyphs
pdfdelta old.pdf new.pdf
pdfdelta old.pdf new.pdf -j result.json
pdfdelta old.pdf new.pdf -o diff.txt
pdfdelta -q -s old.pdf new.pdf
pdfdelta old.pdf new.pdf --color auto|always|never
pdfdelta old.pdf new.pdf --old-password-file old.secret --new-password-file new.secret
pdfdelta old.pdf new.pdf --old-font-identity FontName=identity --new-font-identity FontName=identity
pdfdelta completions bash|zsh|fish|powershell|elvish
```

`inspect`にはbackend確認用の`--backend-info`、object確認用の`--objects`、Glyph確認用の`--glyphs`を提供する。入力PDFのパスには標準入力（`-`）も使用可能（ただしold/newの両方に指定することは不可）。また、テキストレポートをファイル出力する`-o, --output <PATH>`、CI等の終了コード判定用の`-q, --quiet`、JSONレポート用の`-j, --json <PATH>`、完全性チェック用の`-s, --strict`、シェル自動補完スクリプトを生成する`completions <SHELL>`を備える。

passwordはargvへ直接渡さず、sideごとのpassword fileから最大4096 byteを読み、一つの末尾改行を除いてbackendへ借用する。password本文はerror、report、traceへ書かない。`--old-font-identity` / `--new-font-identity`は`BaseFont=identity`形式のcaller assertionであり、identity文字列をdomain-separated hashへ変換した後は保持しない。同じ外部font programを使うとcallerが保証できる場合だけ指定する。

exit codeはCI利用を前提に定義する。

| code | 意味 |
|---|---|
| 0 | 対応するテキスト範囲の比較が完全で、確定した内容変更がない |
| 1 | 対応するテキスト範囲の比較が完全で、確定した内容変更がある |
| 2 | 実行不能なerror(I/O、fatal parse error等) |
| 3 | 結果を返せるが比較が不完全。候補、未解決範囲、抽出の欠落、位置が未確定の変更領域を含む |

判定優先順位は`2 > 3 > 1 > 0`とする。
不完全な比較は、確定した変更があっても既定で3を返す。
JSONでは変更の有無と比較の完全性を独立した値として返す。
`--strict`は新しい既定動作の互換aliasとして受理し、quiet modeにも同じ終了条件を適用する。
現行実装はstrict modeだけが不完全時に3を返すため、この変更は既存scriptに影響する。
実装時にhelp、テスト、移行文書を揃えて更新する。
coreは比較結果の状態を返し、process exit codeへの変換はCLIが所有する。
テキスト比較の完全性は、画像の比較や見た目の一致を意味しない。

---

## 6. PDF Backend と Primitive Extraction (Track A)

### 6.1 目的

PDFからplain textを取り出すことではなく、後から検証可能なraw evidenceを保ったまま、文字・描画位置・描画順を復元することが仕事である。

### 6.2 既存PDF Parser backend

初期版は既存parser libraryを`PdfParser` adapter越しに使用する。backendの必須capabilityは次の通りである。

- Header / version、Indirect Object、reference解決。
- classic xref tableとxref stream。
- object stream(ObjStm)。
- trailer chainとincremental updateのlatest revision解決。
- Page Tree走査とResourcesの継承。壊れた枝にParent不整合または参照循環があっても、独立して検証できた正常枝は保持し、欠落枝とroot `Count`不一致をdocument-scoped UNRESOLVEDとして返す。通常実行は部分証拠からreportを生成できるが、`--strict`はcomparison不完全としてexit 3を維持する。
- raw stream metadataとdecoded stream bytesの取得。
- 初期必須filterとしてFlateDecode。未対応filterは空文字へ潰さずUNSUPPORTEDにする。
- 暗号化の検出。既存backendがempty user passwordで自動復号を完了した場合、またはcallerが明示passwordを借用で渡して復号できた場合だけ受理する。password欠落・不一致、復号後も`Encrypt`が残る文書、未対応security handlerはUNSUPPORTEDにする。CLIはpassword fileのpathだけを受け取り、password本文をreportやtraceへ保持しない。
- object count、recursion、decompressed size等のresource limitと、error categoryの保持。

adapterはobject id、generation、dictionary key、stream filter、Content Stream順序を保持する。library独自型は`pdf/backend/`内で中立型へ変換し、後段へ漏らさない。

A0では候補backendを同じcapability fixtureへ通し、要件を満たす既存libraryを一つdefaultに固定する。選定は人気やAPIの好みではなく、fixture通過率、raw evidenceへのaccess、安全なlimit実装可能性で決める。

### 6.3 Content Stream interpreter と座標

Content Streamの構文解釈とText / Graphics State追跡はpdfdelta側で実装する。初期実装で扱うoperatorは次の通りである。

```text
q Q cm
BT ET Tf Tm Td TD T* TL Tj TJ ' " Tc Tw Tz Ts Tr
Do
```

`q` / `Q` / `cm`でGraphics StateとCTMを追跡する。`Do`ではXObject subtypeを解決し、ResourcesとMatrixのstackに上限を設けた上で、Form XObjectだけを再帰的に解釈する。Formは呼び出し元から分離したstate snapshotで実行するため、対応する`Q`がない`q`はForm境界でのみ破棄してよい。一方、`Q`のunderflowとPage単位のstack不均衡はUNRESOLVEDのままにする。Image XObjectは画像比較が初期scope外であるため明示的にskipする。`'`と`"`は対応する複合text operationへ展開してから解釈する。

Pageの`/Contents`が複数streamのarrayである場合は指定順に解釈し、完成済みtokenをstream境界で連結しない。pending operandはarray全体で保持し、dictionary keyのvalueが次のstreamから始まる場合だけ限定的に回復する。未完成operandのbufferと再parseは1回に制限し、次のstreamでも完成しなければ、同じprefixを繰り返し処理せずUNRESOLVEDとする。末尾sequenceに対する投機的なoperand node課金はrollbackし、parse成功時に1回だけ課金する。sequence末尾でvalueが欠ける場合、通常の不正operand、resource limit超過はerrorのままにする。`BI` / `ID` / `EI`は専用inline-image lexerで処理し、後続operatorから解釈を再開する。

Page boxは`/Rotate`を適用する前に各axisを`min`と`max`で正規化する。有限な座標の大小が逆転している場合は受理し、面積がゼロまたは非有限のboxはUNRESOLVEDとする。

CTM、Text Matrix、Text Line Matrix、Font Matrix、horizontal scaling、character / word spacing、text riseを追跡し、最終Glyph座標とadvanceを求める。Text Render Modeは初期から記録する。non-painting modeのGlyphはevidenceとして保持するが、初期のvisible-content比較からは除外する。clipping、alpha、白塗り上書き等を含む完全なvisibility判定は§10.3で追加する。

### 6.4 Font decoding は独立した難所として扱う

glyph code → Unicodeは単純な変換ではない。ToUnicode、simple font、Type0 font、CIDを順次扱う。`pdf/font/` moduleとして分離し、codeの分割・mapping・advanceを一つの結果として返す。

```rust
pub enum DecodedText {
    Mapped(String),
    Unmapped { font_hash: FontProgramHash, glyph_id: u16 },
}

pub struct DecodedCode {
    pub raw_code: Vec<u8>,
    pub text: DecodedText,
    pub glyph_id: Option<u16>,
    pub advance: f64,
}

pub trait FontDecoder {
    fn decode_codes(&self, bytes: &[u8]) -> Result<Vec<DecodedCode>>;
}
```

Unicodeへ戻せないglyphをU+FFFDや空文字で潰さず、`Unmapped`として保持することを仕様とする。

これには理由がある。born-digital PDFでも、subset fontでToUnicodeを持たないものはある。これらを一律UNRESOLVEDに落とすとCoverageが実用にならない文書が出る。一方、diffツールに必要なのは必ずしも「読める文字列」ではなく「同一性判定」である。old/newが同一のfont programを埋め込んでいれば、`(font_hash, glyph_id)`を比較tokenとしてmatchingとdiffが成立する。

font programが異なりUnicodeにも戻せない場合、その領域はUNRESOLVEDとする。raw code、font object、Content Stream上のprovenanceはreport/debug用に保持する。

simple fontおよびCID descendantの`FontDescriptor`でAscent / Descentが欠落するか、`Ascent <= Descent`となってcross-axis extentを構成できない場合は、妥当な`FontBBox`があればそのtop/bottomをfont-wide vertical metricとして用いる。どちらからも正のextentを得られない場合だけUNRESOLVEDとし、ゼロ面積Glyphを後段へ渡さない。

simple fontではcodeごとに完全一致するToUnicode entryを優先し、entryが存在しない場合だけDifferencesから宣言済みのStandard、WinAnsi、MacRoman encodingの順にfallbackする。明示された不正entryは`Unmapped`のまま保持し、fallbackしてはならない。明示された未知のDifferencesもunmappedのままとし、font identityで暗黙に回復してはならない。Standard 14 fontではcanonicalなBaseFont名と、組み込みencodingまたは明示されたStandard / WinAnsi / MacRoman encoding名をstable identityへ含める。Differences dictionaryはcode selectorの意味を変えるため、このcanonical identityの対象にしない。Type1Cのidentityは、subtypeが`Type1C`である単一の`FontFile3` streamを必要とする。MMType1はvariation axisを解釈せず、宣言済みsimple-font encodingとmetricsだけを使う。選択されたdesign instanceをidentityへ含められるまでは、unmapped glyphにstable identityを与えない。

定義済みIdentity-HとIdentity-Vは常に固定2-byte codeとしてcontentを分割する。custom Type0 Encoding CMapは、`WMode`が0または1で、単一のfull-domain codespace (`<00> <FF>`または`<0000> <FFFF>`) と、同じ範囲をCID 0から写す単一のidentity `begincidrange`だけを持つ場合に限り、固定1-byteまたは2-byte codeとして扱う。domain全体をentry budgetへ課金し、`usecmap`、`begincidchar`、複数range、非identity mappingはUNSUPPORTEDとする。

ToUnicodeはdecoderの固定幅で完全一致参照する。decoder幅のdomainから完全に外れたcodespaceは、利用可能な同幅codespaceが別に存在する場合だけ無視し、実際の分割幅を変えない。利用可能なcodespaceが一つも残らない場合はUNRESOLVEDとする。entry欠落、孤立したUTF-16 surrogate destination、ToUnicode自体の欠落は`Unmapped`とする。一方、空、奇数長、非hexのdestinationはerrorのままにする。Unmapped entryもCMap entry数、work量、出力scalar数のbudgetへ課金する。unmapped codeを比較可能にするのはdescendant fontがstable identityを提供できる場合だけとし、それ以外は実際にcodeが使われた箇所を文脈付きUNRESOLVEDとする。埋め込みfont programもToUnicodeもないCID fontは、BaseFont名だけでglyph同一性を推測しない。ただしcallerがsideごとに同じBaseFontへ同じ外部font identityを明示した場合は、そのidentityを専用domainでhashし、unmapped glyphのfont identityとして使用できる。これはfont discoveryではなくcaller assertionであり、未指定font、異なるidentity、simple fontには適用しない。entry数、BaseFont byte数、identity byte数に上限を設ける。

Type 3 fontは、次の上限付きsimple-font subsetだけを扱う。

- `FontMatrix`は有限かつ非退化で、`a > 0`、`d != 0`を満たすaxis-alignedな`[a 0 0 d 0 0]`とする。WidthsとMissingWidthは`a * 1000`、FontBBoxのvertical座標は`d * 1000`で正規化する。`d`が負の場合は宣言されたvertical axisを反転し、extentを失わないよう上下を入れ替える。
- `FirstChar`、`LastChar`、`Widths`、`FontBBox`、`Encoding`、`CharProcs`を必須とし、既存のentry、indirection、decoded byte、glyphの各上限を適用する。rotation、shear、translation、horizontal reversalを含むmatrixはUNSUPPORTEDのままにする。
- Unicode mappingにはToUnicodeと既知のAdobe glyph nameを使い、未知nameはunmappedのままにする。DifferencesのnameはCharProcsへ解決できなければならない。text extractionではCharProcのdrawing operatorを解釈しない。
- unmapped glyphでは、間接参照されたdecode済みCharProc streamをglyph nameのbyte順に並べ、Type 3専用domainでhashする。この安定したname順をglyph IDに使い、dictionary順、object ID、Encoding codeの再配置にidentityが依存しないようにする。font Resourcesが欠落または空、もしくは標準`ProcSet` nameの上限付きarrayだけを持つ場合は、CharProcだけからidentityを生成する。named resourceへ依存する場合は、参照先をobject IDに依存しないcanonical graphとしてhashし、dictionary key、name、scalar、array、decode済みstream bytesをidentityへ含める。streamの`Length`、`Filter`、`DecodeParms`はdecode後の描画内容を変えないtransport情報として除外する。参照先はmemoizeし、循環、graph深度、identity byte数を明示的な上限で拒否する。resource graph内のfontは、FontDescriptorを持たないStandard 14 Type 1か、同じ規則でgraph全体をhashできるType 3に限定する。未埋め込みの非Standard 14 font、direct stream、decode不能stream、CharProcの欠落・曖昧・上限超過がある場合はidentityを生成しない。FontMatrixとWidthsはglyph token identityではなくgeometry evidenceとして扱う。

Identity-Vは、単一のCIDFontType0/CIDFontType2 descendant、固定2-byte code、完全一致で参照する任意のToUnicode、per-CIDの`W2`を持たないsubsetを扱う。前述のfull-domain identity条件を満たすcustom Type0 CMapでは、`WMode 1`も同じvertical subsetへ接続する。`DW2`は省略時の`[880 -1000]`またはdownward displacementを持つ有限な2要素arrayを受理する。vertical originは`(horizontal_width / 2, DW2[0])`、advanceは`DW2[1]`から構成し、bbox、direction、character / word spacing、`TJ` adjustmentをvertical axisへ適用する。一般のcustom vertical CMap、per-CIDの`W2`、一般的なvertical reading orderは初期scope外とする。

---

## 7. Layout Reconstruction (Track B)

### 7.1 Glyph → Line

文字進行方向 d=(dx,dy) とその垂直方向 n=(-dy,dx) を取り、Glyph座標を両方向へ射影して along-axis / baseline-axis 位置として比較する。data modelは任意方向を保持するが、§2.2の初期完成条件は横書き文書で評価し、縦書き固有のfont metricsとreading orderは後続benchmarkで追加する。

同一Line候補かどうかは、baseline距離、glyph高さ、フォントサイズ、書字方向、文字間隔から判断する。固定pixel閾値は使わず、`baseline_distance < 0.25 * median_glyph_height` のような相対値を候補とする。具体値はSPECで固定せず、benchmark(§12)から決める。

**Space再構成**：Line内で隣接glyph間のalong-axis gapが、フォントサイズと平均advanceに対する相対閾値を超える場合、spaceを挿入する。space glyphを描画しないPDF(英語文書に多い)でCase 1を解くための必須処理である。閾値はbenchmarkから決める。

### 7.2 Line → Block

Line間の接続score S = wv·V + wx·X + wi·I + wf·F を計算する。V=垂直近接、X=水平重なり、I=インデント類似、F=フォント連続性。

page boundaryをBlock boundaryとして固定しない。初期の1段組では、前page末尾と次page先頭のLineも、本文領域・indent・font・line gapの正規化値が連続する場合は同一Block候補にする。繰り返しheader/footerは本文候補から除外し、確信が持てない場合は分割したままAlignmentの1:2/2:1へ渡す。

初期段階では文章内容を主判定に使わない。「『。』で終わったから段落終了」のようなルールだけで決めず、layout情報を先に使う。page boundary継続の曖昧性を救うための軽い文字種・句読点signalは補助として記録してよいが、それだけでBlockを確定しない。

### 7.3 Region (後半のstageで実装)

- **XY-Cut**：中央のvertical whitespaceを分割候補とする。ただし column detected = reading order determined ではない。Region構造とReading Orderは別物として扱い、決められない場合はUNKNOWNのまま残す。
- **Region Graph**：TreeではなくAbove/Below/LeftOf/RightOf/Aligned/SameColumnの関係を持つgraphとして保持する。
- **表**：罫線がある場合はvector drawing operatorから`VectorLine { from, to, width }`を取る。罫線なし表はx/y方向の繰り返しalignmentから推定する。表は通常Blockと同じ方法で直列化しない。

---

## 8. Normalization

一つのBlockから三種類の文字列を作る。ただし、文字列だけを保存してGlyphとの対応を失ってはいけない。

```rust
pub struct BlockText {
    pub raw: MappedText,        // PDFから復元したままの文字列
    pub canonical: MappedText,  // 「文章として同一」とみなす正規化
    pub matching: String,       // Alignment専用。diffには絶対に使わない
    pub normalization_events: Vec<NormalizationEvent>,
}

pub struct MappedText {
    pub text: String,
    pub source_map: Vec<SourceMapEntry>,
}

pub struct SourceMapEntry {
    pub output_range: ScalarRange,
    pub source: TextSource,     // Glyph範囲またはglyph間に挿入したsynthetic space
}
```

`ScalarRange`はUTF-8 byte offsetではなくUnicode scalar valueのindexで定義する。ligature展開、NFC、hyphenation結合で1 glyph ↔ N文字またはN glyph ↔ 1文字になっても、canonical範囲から元Glyphとgeometryへ戻れることを必須とする。`matching`はExact DiffやChange spanに使わないため、完全なsource mapを要求しない。

### 8.1 canonical の内容

canonicalで吸収するのは、文章としての同一性に影響しない差に限る。

**吸収する**：Unicode正規化(NFC)、soft line breakの文脈依存join、連続空白の単一化、行末ハイフネーションの結合、ligature展開。

Soft line breaks are not uniformly deleted. Latin letter/digit boundaries normally insert one space, while CJK and mixed CJK/alphanumeric boundaries concatenate; explicit whitespace is not duplicated. A source U+00AD discretionary hyphen at a line end may be removed with its break. Ordinary `-` and U+2010 are retained: character shape or word length cannot prove discretionary hyphenation. When a lexical interpretation is uncertain, the hyphen source range remains unresolved. Paragraph boundaries remain recoverable block boundaries.

**Independent interpretation evidence:** The existence of an interpretation that matches the other document is not, by itself, evidence that the interpretation is correct. Matching hypotheses may improve recall, but Exact Diff may consume only independently justified source/layout interpretations. Every inter-block boundary is decided independently using retained whitespace, script rules, and source line positions. Unknown joins remain unresolved even when a candidate variant matches exactly.

Canonical insertion boundaries use the complete contributing raw source set. An exact boundary is distinct from a boundary inside a shared source (such as an expanded ligature) or an ambiguous interval containing deleted evidence. NFC composition must project its trailing boundary after every contributing scalar. Source merging preserves first-occurrence order and switches from bounded linear scans to set-based deduplication for large runs.

Ordered-alignment uncertainty belongs to correspondences, not edit-operation order. In ambiguous intervals, a match is retained only when the best complete path excluding that match has a sufficient score deficit. These replays consume the shared DP cell budget; exhaustion leaves unproven ranges unresolved. Heading candidates retain weak or missing evidence independently of section acceptance; multiline or non-prominent candidates do not weaken the existing proof required for a section or Move.

**吸収しない**：全角/半角の差(NFKC相当の互換分解)。全角半角の統一は識別子、型番、契約番号などで意味のある改訂になり得るため、暗黙に同一視してはいけない。NFKCではなくNFCを採用するのはこのためである。

canonicalで差を吸収した場合、`NormalizationEvent { kind, raw_range, canonical_range, source }`として記録する。Alignment後、対応Block集合のold/newでrawは異なるがcanonicalが等しいeventをFormatting-onlyカテゴリ(§5.3)に計上する。隣接する同種eventは一つへmergeし、Glyph数やLine分割数の違いだけで件数が増えないようにする。rawに差があったのに出力上なかったことになる、という状態を作らない。

### 8.2 matching と数値マスク

matchingはAlignmentだけに使う。全角半角の同一視、および `Release 10` / `Release 20` を対応させるための数値マスク(`Release <NUM>`)をここで許可する。

**マスクの安全策**：数値密度の高いBlock(価格表やrelease一覧など)では、マスク後の文字列が行間でほぼ同一になり、誤った1:1対応から「一見正しい誤diff」が生まれる。これを防ぐため次を仕様とする。

- Blockのmatching文字列に占めるマスク由来文字の割合に上限を設け(初期値30%、benchmarkで調整)、超えたBlockはマスクなしmatchingへフォールバックする。
- マスク一致のみによる対応付けは確定させない。anchor鎖内の位置整合またはneighbor consistency(§9.6)による裏付けを必須とする。

---

## 9. Cross-document Alignment

このプロジェクトで最も重要な独自実装である。candidate generationと最終matchingを分け、近似indexの誤りがそのままChange判定にならない構成にする。

### 9.1 Anchor 検出

一意性が高く確実な一致をAnchorとして探す。候補：長い完全一致Block、一意な見出し、条番号(「第十二条の二」)、節番号、表番号、稀な文字列。old/new双方に一度ずつしか現れない文字列が強いanchorになる。

anchorの文字列が一意でも、周辺領域の対応や読み順まで自動的に確定するとは限らない。
区間の確定には§16の境界、競合、探索範囲の条件を適用する。

### 9.2 Anchor の順序整合と move 候補

old→newのanchor対応列に対し、new側indexのLongest Increasing Subsequenceを取り、main chainとする。局所的に同じ文字列が現れても、文書全体の順序から不自然な対応を除外できる。

LISから外れたanchorはmove候補として保持する。
`ChangeKind::Move`の確定には、内容の完全一致、出現箇所の曖昧さのない対応、移動を示す前後の順序関係を必要とする。
scoreだけではmoveを確定しない。
根拠が不足する場合はmove候補または未解決領域として残す。
deletionとinsertionに分けて確定する場合も、それぞれの対応領域が確定条件を満たす必要がある。

### 9.3 Fuzzy Matching とBlockFeatures

Embeddingは初期実装では使わない。文字n-gram(初期は3-gramのset)を使い、最終的なtext類似度はDiceで計算する。

```rust
pub struct BlockFeatures {
    pub exact_hash: ExactHash,
    pub ngrams: NGramSet,
    pub normalized_geometry: NormalizedRect,
    pub style: StyleFeatures,
    pub anchor_interval: Option<AnchorIntervalPosition>,
}
```

absolute page座標は改ページやreflowで大きく変わるため、positionは補助signalである。利用する場合はpage内の正規化座標や、前後anchor間での相対位置を使う。

### 9.4 Candidate Generation：Inverted Index とoptional LSH

全Block組を比較しない。candidate generationはtraitで切り離す。

```rust
pub struct Candidate {
    pub block: BlockId,
    pub sources: Vec<CandidateSource>,
    pub coarse_score: f64,
}

pub trait CandidateGenerator {
    fn candidates(
        &self,
        old: &BlockFeatures,
        limit: usize,
    ) -> Vec<Candidate>;
}
```

初期defaultは、new側に`HashMap<NGram, Vec<BlockId>>`を作るn-gram inverted indexである。共有n-gram数とIDF相当の希少性から候補を絞り、最終Dice scoreは候補に対して別途計算する。

LSHは長文書でcandidate数が膨張した場合のoptional実装として検証する。初期候補は、character n-gram setに対するMinHash + bandingである。DiceとJaccardはset上で単調対応するため、MinHashは現在のtext類似度と整合しやすい。

ただし、contentとabsolute positionを一つのhashへ強制的に混ぜない。改ページ、margin変更、段落moveでpositionが変わるとtrue matchを候補から落とすためである。positionを使う場合は次のいずれかに限定する。

- content LSHとは別のcoarse geometry bucketを作り、candidate集合をunionする。
- anchor interval内の相対位置をreranking signalにする。
- move検出ではgeometry条件を外したcontent候補も必ず残す。

candidate集合は`exact/anchor候補 ∪ inverted-index候補 ∪ optional LSH候補 ∪ move候補`とする。LSH collisionや同一bucketであること自体はconfidenceへ加点せず、最終scoreとneighbor consistencyで検証する。LSHは候補生成の高速化であり、matchingやExact Diffの判定器ではない。

### 9.5 Matching Score

重みは固定せず、利用するsignalだけを仕様とする：text類似、geometry類似、style類似、neighbor類似、anchor文脈。最も強いsignalはtextとする。改ページや段組変更で同じ文章が全く違う位置へ移動しうるため、geometryは補助にとどめる。

candidate generator由来の情報は「候補へ入った理由」としてdebug出力に残すが、それだけでmatching scoreを上げない。数値マスク一致やgeometry一致だけによる確定も禁止する。

### 9.6 Robust Alignment

alignmentとrecoveryの出力は、共通の対応付け判定を通過してから、確定した変更または解決済みの同一範囲になる。
scoreとneighbor refinementは候補を作り、確定時には原文への逆写像、対応領域の閉包、抽出と読み順の前提、競合する対応、主張に必要な探索の完了を確認する。
top-k探索の打ち切りを、対応する内容が存在しない証拠にはしない。
coreの比較APIを直接呼ぶ場合も同じ判定を通す。
対応領域の閉包と有界なrecoveryの規則は§16で定義する。

B3では1:1、1:0、0:1に加え、同一anchor区間内で隣接Blockだけを結合する制約付き1:2 / 2:1を扱う。これは§2.2 Case 1/2で、line wrapやpage boundaryにより片方だけBlockが分割された場合に必要である。B4では1:3 / 3:1を追加し、より一般のDynamic Programmingへ拡張して構造化誤差を吸収する。

Matchingは一回で確定しない。`A X B` / `A Y B`のように前後(`A ↔ A`、`B ↔ B`)が確定した後に`X ↔ Y`のscoreを引き上げる、initial matching → neighbor consistency → refinementの二段階とする。

### 9.7 Candidate generatorの採用条件

小規模documentでは全Block pairをscoreするexhaustive実装をtest oracleとして保持する。inverted indexまたはLSHは、true alignmentがcandidate上位K件に残るrecall、候補件数、latency、memoryをこのoracleと比較する。

LSHは、holdoutを含むbenchmarkでcandidate recallを悪化させず、候補件数または実行時間を明確に改善した場合にだけdefault化する。改善がなければtrait実装は残してもdefaultはinverted indexのままとする。

---

## 10. Exact Diff とその先

### 10.1 Myers Diff

構造上有効な対応候補に対して、Myersによるexact diffを実行する。
共通判定を満たす対応からは確定した変更を出力し、根拠が不足する対応から得た編集は、原文を保持した差分候補として出力できる。
編集列がexactであっても、その前提となる対応付けの正しさまでは証明しない。
character-levelからtoken-levelでの位置特定と変更tokenのexact比較へ進める方針を維持する。

diffの入力はcanonical文字列(またはUnmapped領域では(font_hash, glyph_id)トークン列)であり、matching文字列は絶対に使わない。

### 10.2 OCR (最後に追加)

OCR結果は専用pipelineにせず `OCR → Glyph[]` へ変換して既存pipelineを再利用する。OCR誤認識らしい差分(`0 ↔ O`、`1 ↔ I`)も消さず、changeに `OcrConfusion` タグを付けて報告する。

### 10.3 Render Awareness (後半)

編集済みPDFでは、old text → 白い矩形 → new text と上書き描画され、内部にold/new双方の文字が残ることがある。将来的にrender order、clipping、fill/stroke、CropBox、可視性まで扱う。ここでは`hayro`のようなinterpreter/rendering実装が参考になる。

---

## 11. ロードマップ

Track A(PDF Backend + Primitive Extraction)とTrack B(diff engine)を並行させる。Track Bは`Document<Glyph>` fixtureで進められるため、parser backendの選定やfont edge caseがAlignment開発を止めない。

### Track A：PDF Backend + Primitive Extraction

| Stage | 実装 | 確認 |
|---|---|---|
| A0 | `PdfParser` / `ParsedPdf`中立境界、capability fixture、既存library比較、default backend選定 | `pdfdelta inspect doc.pdf --backend-info` |
| A1 | 既存parser adapter、xref(table + stream)、ObjStm、trailer chain、Page Tree、Resources、stream decode、limit/error分類 | `pdfdelta inspect doc.pdf --objects` |
| A2 | Content Stream parser、Graphics/Text State、Form XObject、ToUnicode、Tj/TJ、Glyph座標、/Rotate正規化 | `pdfdelta inspect doc.pdf --glyphs` |
| A3 | Debug renderer：Glyph overlay SVG、canonicalページ座標、object/operator provenance | `pdfdelta inspect doc.pdf --svg debug.svg` |
| A4 | font decoding拡張(Type0、CID、Unmapped token)とDifferential Test | §12.4のconformance suite |
| A5(optional) | alternate/custom `PdfParser` backend | §3.3のtriggerが再現し、同一suiteを通過 |

A3のSVG上で、PDFに実際に見えている文字と復元したGlyphが正しい座標で重なり、各GlyphからContent Stream/operatorへ戻れる状態を、Track Aの最初の観察可能なゴールとする。

A5は既定のマイルストーンではない。A0で選んだ既存backendをproductionで使い続け、具体的な失敗が出た場合にのみadapter差し替えまたはcustom backendを追加する。

### Track B：Diff Engine

| Stage | 実装 | 完成条件 |
|---|---|---|
| B0 | Workspace骨格、GlyphExtractor境界、Glyph document fixture、error types、parser-backed composition | `pdfdelta --help` + Glyph fixture test |
| B1 | Glyph → Line(space再構成含む) | 1段組/多フォントサイズ/上付き/日英のbenchmark |
| B2 | Line → Block | 段落/見出し/改ページ/spacing差のbenchmark |
| B3 | canonical正規化、exact anchor、CandidateGenerator trait、n-gram inverted index、1:1 + 制約付き1:2/2:1 alignment、insert/delete、Myers Diff、Changeスキーマ、exit code | §2.2の5ケース(最初の実用版) |
| B4 | 1:3/3:1を含むgeneral DP alignment、neighbor consistency、move検出(LIS外れanchorの再評価) | ParagraphMoveと複雑なBlock split/mergeを含むbenchmark |
| B5(optional) | MinHash LSH candidate generator、large-document profiling | §9.7を満たす場合だけdefault候補 |

B4が最も重要な技術的マイルストーンである。B5はcorrectness機能ではなくscalability改善であり、B3/B4を先に成立させる。

### 後続 Stage (Track合流後)

未知PDFへの対応改善は、§16の共通判定、構造回復、出力の分離、未使用文書による評価の順で進める。
この作業は既存の比較pipelineを強化するものであり、五つの初期受入条件を置き換えない。

| Stage | 内容 |
|---|---|
| C1 | Multi-column：XY-Cut、Region Graph、UNKNOWN fallback。完成条件は通常の2段組で左右が混ざらないこと |
| C2 | Parallel / Table / Form：新旧対照表、vector line、grid推定、label/value |
| C3 | Render Awareness：z-order、clipping、可視性、CropBox |
| C4 | OCR統合 |

### 最初に実装しないもの

OCR(C4まで)、LLM、Embedding、semantic similarity model、visual pixel diff、高度な表認識、AcroForm、annotation diff、full PDF 2.0、GPU、並列処理、WASM、Web UI、性能最適化、PDF object parserの自作。

最初は、既存parserから得たraw evidenceを失わず、正しい構造と誤りを観察できることを最優先する。

---

## 12. テストと検証

### 12.1 Benchmark Generator (pdfdelta-bench)

Canonical Document Spec(YAML)からPDFを生成する。

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

生成には複数系統のrendererを使う(HTML/Chromium、Typst、LaTeX、custom Rust writer)。単一writerのPDFだけでテストすると、そのwriter固有の内部構造へoverfitするためである。low-level fixtureには`pdf-writer`、通常文書fixtureには`krilla`/Typstを参考にする。

### 12.2 Mutation Engine

Canonical Documentへ二種類のmutationを適用する。

- **Semantic Mutation**(正解diffを発生させる)：TextReplace、TextInsert、TextDelete、NumberReplace、ParagraphInsert、ParagraphDelete、ParagraphMove
- **Rendering Mutation**(content diffを発生させない)：FontSizeChange、MarginChange、PageSizeChange、LineHeightChange、LineBreakChange、PageBreakChange。`ColumnChange`はC1 benchmarkで有効化する。

これにより「見た目は大幅変更 + `Release 10` → `Release 20` だけ内容変更」のようなケースを自動生成する。

evaluatorは、報告されたChangeと期待Changeを、kind一致 + span重なり(閾値はIoUで定義)で照合する。この照合ルールとChangeスキーマ(§5.1)が確定していることがevaluator実装の前提であり、B3より前に固定する。

### 12.3 実データベンチマーク

合成データだけではpdfdelta-bench固有の癖にoverfitする。real-world評価には、公開されている規程、policy、report、manualなどの改訂ペアを、利用条件を確認した上でsourceとして使う。ただし公開説明が必ずしもexact spanや全変更を機械可読で与えるとは仮定せず、採用pairごとに人手でreviewしたexpected manifestを作る。

調整には合成fixtureと、開発用に割り当てた実文書のグループを使う。
人手でannotationを付けただけではholdoutにならず、改訂版、抜粋、共通template、派生文書は同じ文書系列として一つのsplitへまとめる。
評価用グループは調整前に固定し、既知のproducer系列を記録して、文書の未知性とproducerの未知性を分けて報告する。
失敗の診断や修正に使った文書は開発用へ移し、以後の汎化評価には未使用のグループを用意する。
完全なannotationから求めるprecisionとrecallは、annotationの範囲に限った値とする。
部分annotationは列挙した変更のrecallを評価できるが、precisionを主張する分母には使わない。
品質評価を省略したpairや抽出に失敗したpairも、実行結果の集計には残す。
照合方法、評価指標、公開条件の詳細は§16に定める。

### 12.4 Differential Testing / Backend Conformance

比較対象の抽象度を混ぜず、二層に分ける。

**Parser backend conformance**：同一fixtureをdefault backendとalternate parserへ入力し、pdfdelta中立型へ正規化した上で、page順、reference解決、resource inheritance、decoded Content Stream bytes、object stream内object、incremental updateのlatest objectを比較する。library固有のdebug文字列やobject配置順そのものは比較しない。

**Primitive Extraction differential**：同一PDFに対し、pdfdeltaの`ParserBackedGlyphSource`と独立したposition-aware extractor / interpreterを比較する。decoded text、Glyph数、text order、bbox、baselineを許容誤差付きで比較し、SVG overlayで人間が確認できるようにする。plain textしか返さないextractorはtextの補助oracleとして使い、geometryの正解とはみなさない。同じunderlying parserを共有するtool同士だけの一致は独立oracleとみなさず、hand-written fixtureまたは別実装で裏付ける。

custom `PdfParser` backendを将来追加した場合も、同じconformance suiteを通す。Differential Testingは自作parserを正当化するためではなく、backendを交換しても上位層の意味が変わらないことを確認するために使う。

### 12.5 Property Testing

`proptest`をテスト用依存として使い、次を検証する。

```text
fixture object → backend adapter → equivalent neutral object
normalize(normalize(x)) == normalize(x)
diff(x, x) == empty
alignment(x, x) == identity
candidate_generator(x) contains identity match
```

### 12.6 Fuzzing とresource limits

PDFはuntrusted inputとして扱う。default parser adapter入口、中立object変換、CMap Parser、Content Stream Parser、FontDecoderをfuzz targetとし、max object count、max recursion depth、max decompressed size、max page count、max glyph count、max Form XObject depth、max nesting depthを制限する。malformed PDFでpanic、無限ループ、無制限のメモリ確保を起こさない。

既存parser libraryを使う場合も安全性をlibraryへ丸投げしない。adapter前後で入力size、decode budget、page/object budgetを管理し、panicやlimit超過をfatal errorまたはUNSUPPORTEDへ分類する。

default limitは有限かつ設定可能な状態を維持する。今回の公開corpus実測に基づき、Content Stream operand nodeはdocument-globalで10,000,000、rawまたはcomparable diff tokenはold/new pair-globalで5,100,000、defaultの3-gram表現はpair-global token elementで15,300,000を上限とする。明示的に低いlimitを指定した場合は、同じ課金境界で必ず失敗させる。default値の再調整は再現可能なfixtureまたはcorpusの証拠に基づく場合だけ行い、文書を通すためにlimit自体を削除してはならない。

cargo-fuzzはnightlyを要するため、fuzz crateのみstable制約(§3.5)の例外とする。

### 12.7 Candidate Generation評価

小規模fixtureではexhaustive all-pairs scoreをoracleとして保存する。合成fixtureのcanonical paragraph idとspan overlapからtrue counterpartを定義し、各candidate generatorについて次を測る。

- true matchがtop-K候補に含まれる割合(candidate recall)。
- old Blockあたりのcandidate数(p50 / p95 / max)。
- index build時間、query時間、memory。
- ParagraphMove、改ページ、margin変更時にposition featureがrecallを落としていないこと。

MinHash LSHのband数、signature長、K等は合成benchmarkで調整し、人手review済みのreal-world holdoutは評価にのみ使う。LSH導入前後で最終Changeの正解率が変わった場合、candidate recall低下をbugとして扱いdefault化しない。

---

## 13. 依存候補・参考実装一覧

初期parser backendはproduction dependencyとして利用し、それ以外はadapter候補、oracle、edge case発見の資料として使う。どのlibraryも上位層へ直接型を漏らさない。

| 対象 | 候補 / 参考実装 | 主に見るもの |
|---|---|---|
| PDF object parser(initial backend候補) | lopdf | Object、xref、object stream、incremental update、raw stream access |
| PDF parser(alternate adapter候補) | pdf-rs | primitive表現、reference解決、test構成 |
| PDF interpreter | hayro | Text/Graphics State、座標変換、clipping、render order |
| Text extraction oracle | pdf-extract | decoded textの補助比較。geometryの正解とはみなさない |
| Layout extraction oracle | pdf-inspector | position-aware extraction、text/scanned判定 |
| Layout/Table | pdfsink-rs | 複雑レイアウト |
| Font decoding | printpdf | ToUnicode、Type0/simple fontのcode width差 |
| Diff correctness | similar | Myers等のedit script妥当性比較 |
| Diff performance | imara-diff | 後半の性能改善(Stage 1では読みやすく正しいMyersを優先) |
| fixture生成(low-level) | pdf-writer | per-character Tj、reversed object order等のadversarial fixture |
| fixture生成(high-level) | krilla / Typst | 別renderer系統 |

parser backendの最終選択は§6.2のcapability fixtureで決める。library名を仕様の中心にせず、必要capabilityと中立境界を仕様とする。

---

## 14. 開発判断の原則

新機能は、「これがないと現在のBenchmarkで何が失敗するのか」を説明できる場合にだけ追加する。

妥当な例：

- Block segmentationミスが主要なFalse Positive → 1:N alignmentを追加。
- 1万Block文書でinverted indexのcandidate数が膨張し、exhaustive oracle比のrecallを維持したままLSHで改善できる → MinHash LSHを追加。
- 既存parser backendが必須fixtureのObjStmを誤解釈し、alternate backendやadapter修正でも解決できない → custom `PdfParser` backendを追加。

行わない例：

- 将来役立ちそう → LLM embeddingを追加。
- PDF parserはいずれ必要そう → 最初からobject parserを自作。
- similar positionとsimilar contentをまとめられそう → correctness検証なしにLSHを最終matchingへ使用。

常に次の順で進める。

```text
実PDFで失敗を確認
→ 原因の層を分類(parser / extraction / layout / candidate / alignment / diff)
→ 最小限の機能またはbackend差し替えを追加
→ exhaustive oracleとBenchmarkで改善確認
```

ロードマップ(§11)自体もこの原則に従う。Track Aは既存parserを起点にraw evidenceを復元し、custom parserは失敗によってのみ駆動する。Track Bはcorrectnessを先に成立させ、LSH等の近似indexはcandidate recallを保てることが確認できた後に導入する。この順序を守ることで、小さく開始しながら初期実装を捨てずに高精度なPDF Diff Engineへ成長させる。

---

## 15. 仕様変更履歴

この節は、仕様を変更した理由と変更箇所を`SPEC.md`自身に残すための記録である。過去分は`git log --follow -- SPEC.md`と各commitのdiffから復元した。詳細な差分は`git show <commit> -- SPEC.md`で確認する。

### 2026-09-08 未知PDFに対する比較設計

- §5：確定した差分と要確認の候補を分離し、候補と位置未確定の領域を解決済みcoverageから除外する仕様を定めた。不完全な比較を既定でexit 3にする移行方針も追加した。
- §9と§10：候補生成、共通の対応付け判定、exact diffの責務を分け、scoreやexactな編集列だけでは対応を確定しないことを明記した。
- §11と§12：既存の受入条件を維持し、文書系列とproducer系列の混入を防ぐ評価方針を追加した。
- §16：対応領域の条件、可逆な構造回復、範囲の所有関係、資源制限、結果の型、互換性の移行、検証matrix、公開条件、実装順序を日本語で記載した。これらは設計変更であり、実装済みであることを示さない。

### 2026-08-24 人間向けreportのunified diff化（本変更）

- §5.3：人間向けtext reportを、exact diff結果の表示層のみの投影として文脈付きunified diff形式へ刷新すると定義した。1行要約、`---` / `+++` file header、1-based page付きhunk header、隣接する`-` / `+`行、有界なcontext(既定32 comparable tokenずつ)、`~ moved`明示、`?`による未解決領域の可視化、近接exact changeの表示上のhunk統合(同一Block集合・同一Block separatorかつ16 comparable token以下の分離)である。変更区間はcomparable token空間で描画し、canonical rangeが零幅のpure-unmapped編集も`<unmapped>`プレースホルダを変更行内に示す。text reportはJSONと同じspan範囲検証を共有し、範囲外spanはどちらのmodeでも失敗する。`Comparison`とJSON reportの機械可読な意味は不変で、ANSI色は`--color auto|always|never`(既定auto、TTY判定)が制御し記号の補助に限定する。

### 2026-08-23 backend依存のupstream復帰とxref再構築取り込み（本変更）

- §6.2：`lopdf`依存をreviewed forkからupstream `J-F-Liu/lopdf`のmain revisionへ戻し、以降にマージされた保護機構をrevision pinへ取り込んだ。xref-stream entry数のdecoded body上限（#561）、object streamの非破壊parse（#562）、非標準`/BrotliDecode` filter（#567）、stream `/Length`不一致の回復（#568）、startxref解決失敗時の有界なxref再構築fallback（#570）である。fork固有の設定可能xref entry上限は存在しなくなるため、load後のobject予算filterが保持object数を引き続き課金する。crates.ioに該当変更が公開されたらrevision pinをreleaseへ置き換える。
- §6.2：非標準Brotli prototype（`pdfjs-brotli-prototype.pdf`）とLength 0が実streamと矛盾するXObject（`pdfjs-multiple-filters-zero-length.pdf`）を、backend対応待ちからstrict自己比較完走へ移行した。ObjStm index内のQDF形式コメントもbackendと同じ規則で無視し、`pdfjs-issue14165.pdf`をfatalから部分成功へ移行した。
- §6.2：公開test corpus由来の実PDF 689件をstrict自己比較へ一括投入して実測した。#570取り込み後は599件が完走、80件がSPEC文書化済みの明示的境界でstrict不完全、10件がfatal backend errorである。panic、hang、resource limit超過の誤発火は皆無だった。残るfatalのうち6件はpypdfでも読めないfuzzed破損、4件は再構築後もcatalog参照先のobjectが欠落する文書であり、xref再構築で救える領域は既に吸収済みである。

### 2026-08-21 残存5件の入力付き対応（本変更）

- §2.1、§3.2、§5.4、§6.2：借用password APIとside別password file入力を追加し、password本文をargv、report、traceへ残さない規則を定義した。`print_protection.pdf`は公開testで指定されたpassword fileを使いstrict自己比較を完走した。
- §6.2：Page Treeの正常枝を保持し、壊れた枝とroot Count不一致をdocument-scoped UNRESOLVEDとして部分成功へ変換する規則を追加した。`Pages-tree-refs.pdf`は通常実行でreport生成まで完走し、strictでは完全性を偽らずexit 3になる。
- §5.4、§6.4：未埋め込みCID fontへside別の明示external identityを与えるcaller assertionを追加した。`ThuluthFeatures.pdf`は`TraditionalArabic`へ同一identityを指定してstrict自己比較を完走した。
- round2 corpusは、明示入力付きstrict完走35件、通常実行のみ部分成功1件、backend対応待ち2件になった。非標準Brotli filterとLength 0が実streamと矛盾するXObjectは、`lopdf` fork / vendorを変更しない決定により本変更では保留した。

### 2026-08-21 残存6件の再検証（`f670b1c`）

- §6.4：named Resourcesへ依存するType 3 fontを一律UNRESOLVEDとする境界を狭め、参照先を上限付きcanonical graphとしてidentityへ含められるsubsetを追加した。参照循環と未埋め込み非Standard 14 fontは引き続きUNRESOLVEDにする。
- `ContentStreamNoCycleType3insideType3.pdf`で、入れ子のType 3、Standard 14 font、Pattern streamを含む有限なresource graphがstrict自己比較を完走することを確認した。
- `fixtures/manifests/real-world-pipeline-round2.tsv`の期待値を更新した。38件中33件がstrict自己比較を完走し、残る5件はpassword必須暗号、非標準Brotli filter、循環Page Tree、Length 0と実streamが矛盾する壊れたXObject、埋め込みprogramもToUnicodeもないCID fontという明示的な境界である。

### 2026-08-21 追加コーパス第2回（`fe2830a`）

- §6.4：decoder幅のdomain外にある余分なToUnicode codespaceを、安全に無視できる条件を追加した。異なる幅のrangeを併記するsimple fontを実測したためである。
- §6.4：1-byte / 2-byteのfull-domain identityに限定してcustom Type0 Encoding CMapを受理し、domain全体をresource budgetへ課金する規則を追加した。
- §6.4：CID FontDescriptorのAscent / Descent欠落時にもFontBBoxを使うfallbackを追加した。
- §6.4：Standard 14 fontのcanonical identityへ明示されたnamed encodingを含め、Unicodeへ戻せないcodeも安全に保持できる範囲を拡張した。
- §6.4：未埋め込みCID font、resource依存Type 3、非identity custom CMapは、名前やraw codeだけからglyph同一性を推測せずUNRESOLVEDとする境界を明記した。
- §6.2および§6.4の既存境界を追加コーパスで再確認した。password必須暗号、非標準Brotli prototype、循環Page Tree、Length 0と実streamが矛盾する壊れたXObjectは、成功扱いせずUNSUPPORTED / UNRESOLVED / fatal errorを維持する。
- 実測結果は`fixtures/manifests/real-world-pipeline-round2.tsv`に期待exit codeとともに固定した。38件中32件はstrict自己比較を完走し、6件は上記の明示的な境界を再現した。

### 2026-08-21 公開PDFコーパス第1回（`ce93776`）

- §6.3：Form XObjectのstate分離を明文化し、Form内に残った`q`だけを境界で破棄する一方、`Q` underflowとPage単位のstack不均衡はUNRESOLVEDとした。
- §6.3：`/Contents` arrayをまたぐdictionary valueの回復条件、operand nodeの一回課金、再parseを次の1 streamまでに制限する二次時間対策を追加した。
- §6.3：大小が逆転した有限page boxを正規化し、ゼロ面積または非有限boxを拒否する規則を追加した。
- §6.4：simple fontの部分ToUnicode、Standard / WinAnsi / MacRoman fallback、明示的な不正mappingとentry欠落の区別を追加した。
- §6.4：Identity-H / Identity-Vの固定2-byte分割、疎なToUnicodeの完全一致参照、ToUnicode欠落時のstable font identity条件を追加した。
- §6.4：Type1CとMMType1のidentity境界を追加し、design instanceを表現できないMMType1はunmapped identityを生成しないことにした。
- §6.4：Type 3の対応範囲をaxis-alignedな有限`FontMatrix`へ拡張し、CharProcを使うstable identityと、named Resourcesへ依存する場合はidentityを生成しない安全条件を追加した。
- §12.6：公開PDF corpusの実測に基づく有限なoperand node、diff token、3-gram elementのdefault上限と、明示的な低上限を維持する原則を追加した。

### 過去の変更

| 日付 | Commit | 変更した仕様 |
|---|---|---|
| 2026-08-21 | `111ffd5` | §2.1にempty user passwordで復号できるPDFを追加し、§6.2にpassword・security handler・秘密情報の境界を定義した。§6.4にFontBBox fallback、初期Type 3 subset、Identity-V subsetを追加した。 |
| 2026-08-20 | `cf1156e` | §3.6でcanonical / matching規則、soft line break、normalization eventを自前実装の責務として明確化し、Unicode crateへ委ねる範囲をNFCとsegmentation primitiveに限定した。 |
| 2026-08-20 | `c95378d` | §1と§2.2の例を一般的なrelease変更へ差し替え、§5.1に複数Blockのseparatorを含む`TextSpan`規則を追加した。§8と§12では数値mask例、benchmark fixture、公開real-world pair、holdout運用を一般化した。 |
| 2026-08-20 | `1db8dd1` | §2.2の受け入れ条件3を`Release 10`から`Release 20`への1 replacementとして明文化した。 |
| 2026-08-19 | `2324f4c` | 初版SPECを追加し、目的、scope、architecture、data model、pipeline、roadmap、benchmark、resource limitの基本方針を定義した。 |

---

## 16. 未知のPDFに対する比較設計

### 16.1 目的と適用範囲

対応する形式の未知のPDFを、文書ごとの数値調整なしで比較できることを目指す。
正しい差分を検出する割合と、比較を解決できる範囲の両方を改善する。
難しい内容をすべて未解決にするだけでは、この目的を達成したとみなさない。

出力は、確定した差分、要確認の差分候補、未解決範囲に分ける。
既存の`proven_changed_regions`は、差分の存在は確認できるが位置を特定できない状態として、位置まで確定した差分と区別する。
差分候補は利用者が原文と照合できる形で表示する。

対象は既存のborn-digital textの範囲とし、横書きの日本語と英語を含む。
未知のproducerや文書系列は、その範囲内で評価する。
画像、スキャン、未対応のPDF機能、抽出失敗を、正常に比較できたテキストと区別する。
OCR、意味モデル、表認識、自作object parser、根拠のない性能最適化は追加しない。
五つの初期受入条件も変更しない。

**確定**とは、抽出、正規化、構造に関する明示した前提のもとで、比較の確定条件を満たした状態を指す。
PDFの任意の解釈、著者の意図、画面上の完全一致を証明するものではない。
誤って対応付けた段落の内部でexact diffが計算できても、その差分は正しくない。
`High`や`TrustedRun`という既存の名称だけを、確定の根拠にはしない。

### 16.2 現行実装との差と変更箇所

以下は実装箇所の対応表であり、変更済みであることを示す表ではない。
ファイルパスはrepository rootからの相対パスとする。

| 変更箇所 | 現在の振る舞い | 設計上の変更 |
| --- | --- | --- |
| `crates/pdfdelta-core/src/alignment/ordered.rs`の`AlignmentOptions` | score、margin、候補数、計算量の上限を使って対応を選ぶ | 探索と順位付けを、対応を確定する根拠から分離し、探索の打ち切りを記録する |
| `crates/pdfdelta-core/src/layout/line.rs`の`LineOptions` | 文字高、font size、advanceなどに対する相対値で行を推定する | 相対値を維持し、境界の不確かさと再構成の選択肢を残す |
| `crates/pdfdelta-core/src/pipeline.rs`の比較処理 | 構造化、抽出の欠落境界、alignment、recoveryを接続する | 対応の判定とその前提を、出力とcoverage集計まで引き継ぐ |
| 同ファイルの`demote_inferred_order_changes` | 推定した読み順に触れる変更を、比較後に`Low`へ落とす | 表示ラベルだけでなく、確定可否と解決済み範囲へ反映する |
| `crates/pdfdelta-core/src/diff/mod.rs`の`Comparison` | 変更、位置未確定の変更領域、Formatting-only、未解決、coverageを保持する | 候補を別collectionにし、解決状態を範囲単位で検証する |
| `crates/pdfdelta-core/src/diff/recovery/ownership.rs` | recovery対象の所有範囲とgap理由を検証する | 同じ検証の考え方を最終結果へ適用する。既存の部分partitionを文書全体のpartitionとはみなさない |
| `crates/pdfdelta-core/src/report/mod.rs`の`summarize` | `Low`の変更もcontent change件数へ含める | 確定結果と候補の件数、比較の完全性を分離する |
| `crates/pdfdelta-core/src/report/json.rs` | schema version 9で結果を出力する | 候補、確定理由、完全性の定義を次のversionで導入する |
| coreの`report::exit_status`とCLIの`compare.rs` | 不完全時のexit 3はstrict modeに限る | coreは状態を返し、CLIが既定で不完全を3へ変換する |
| coreの`source.rs`、`pdf.rs`、`error.rs` | 抽出問題の範囲、parser上限、エラー分類を保持する | 局所的な回復とreportでも分類を維持し、失敗を空文字へ置き換えない |

### 16.3 候補生成と確定判定の分離

既存のcandidate generatorとMyers diffを使い、候補となる対応と確定結果の間に共通の判定処理を置く。
最初はprivateな具象型と検証付きconstructorで表現し、新しい公開plugin traitや別のPDF modelを導入しない。

```text
raw PDF evidence
  → 元の情報を保持した構造と正規化
  → 対応候補の生成
  → 対応範囲、競合、探索の完了状態の判定
  → 対応候補内のexact diff
  → 確定した変更／要確認の候補／未解決範囲
  → 範囲の所有関係からcoverageと完全性を集計
```

1. glyphと正規化のsource mapを保持する。描画されたspaceと合成したspaceを区別し、行結合と読み順の前提を記録する。
2. anchor、index、score、有界なrecoveryから候補を作る。探索した範囲と打ち切りを記録し、探索に出なかった内容を直ちに削除とは判定しない。
3. 変更候補と同一候補の両方について、原文への逆写像、抽出の不確かさ、読み順、所有範囲、競合する対応、主張に必要な探索の完了を判定する。
4. 構造上有効な候補にexact diffを適用する。対応が未確定でも、原文の位置が有効なら編集列を差分候補として保持できる。
5. 確定した変更と同一範囲を共通の出力処理に渡す。候補は未解決範囲への注釈として残し、この判定後にcoverageを集計する。

共通判定の対象は、通常のordered match、挿入、削除、move、sentence recovery、page anchor recovery、複数出現箇所のgrouping、`compare_aligned`の直接呼び出しを含む。
診断用やshadow用の処理が、この判定を迂回して確定結果を出すことは認めない。
report生成時に`Low`だけを除外する実装では、候補だった同一範囲のcoverageや、低信頼ではない誤対応が残るため、この設計を満たさない。

### 16.4 対応の根拠と閉じた比較領域

**対応領域**は、新旧間で対応を主張する内容の範囲と、その境界を組にしたものとする。
判定は以下の情報を保持する。

| 情報 | 保持する内容 |
| --- | --- |
| 原文範囲 | old/newごとのblock、comparable token、canonical scalarの範囲と、glyph/pageへの逆写像 |
| 比較の前提 | 正規化policy、Unmappedのfont identity、合成space、採用した読み順と境界 |
| 対応領域 | 比較する内容、領域の境界、その境界が改訂間で対応する根拠 |
| 探索状態 | 調べた競合候補、探索の打ち切り箇所、一意性を支える完全探索またはexact検証 |
| 判定 | 確定、候補、未解決の状態と、構造化した理由および根拠への参照 |
| 依存関係 | 判定が依存する抽出、境界、読み順、先行する対応。循環した根拠で互いを確定しない |

**領域が閉じている**とは、主張を変え得る内容や競合する対応を、その領域から根拠なく除外していないことを指す。
似た見出しが二つ見つかったことや、同じpage番号であることだけでは領域は閉じない。
文書の他の場所に対応がないことを根拠にする場合は、その場所まで探索するか、未確定として残す。
小さな範囲の探索結果で、より広い範囲について一意性を主張しない。

対応領域は外側から内側へ構成する。
抽出が完全で、対応する単一の読み順を裏付けられる文書では、文書全体を最初の順序付き領域にできる。
その読み順も比較modelの前提として記録する。
複数領域の文書では、全体を一つの読み順へ押し込まず、根拠のあるregion/runの関係から始める。

確定した親領域の中で、境界候補のexactな出現箇所を必要なtoken範囲全体で調べ、交差する割り当てや競合を除き、境界間の内容を欠落なく分割する。
子領域は親の未解決な前提を引き継ぐ。
親や境界の対応を確定できない場合、recoveryは候補を提示できるが、領域が閉じたと見なして処理を進めない。

探索の完全性は、明示した対応modelの範囲で定義する。
modelには採用する構造上の順序制約、取り外せるsoft境界、区間を区切るexact anchor、繰り返し出現の扱いを含める。
PDFのあらゆる解釈を列挙したという意味ではない。
類似度の足切りやtop-kだけで競合を定義から除き、確定を正当化することは認めない。
出現箇所の確認には既存のexact indexと実tokenの照合を使い、区間内の選択肢は有界なalignmentで調べる。
必要な探索を完了できなければ、その主張は未確定のまま残す。

同じ前提で成立する編集位置が複数あり、報告する種類または原文範囲が異なる場合は候補として残す。
複数のheuristicが一致しても、それだけで独立した証明とは扱わない。
既存のheuristicから引き継いだ前提と、exact検証で確認した事実を判定理由で区別する。

### 16.5 結果ごとの確定条件

| 結果 | 必要な条件 | 不足する場合 |
| --- | --- | --- |
| 同一 | 確定した領域と読み順の中でcomparable token列が完全一致し、原文と正規化の前提が有効 | 候補または未解決。文字列一致だけで解決済みにしない |
| 置換 | 領域の対応、exactな変更範囲、source mapが有効で、種類や位置を変える未解決の競合がない | 置換候補、または変更の存在だけを確認できる領域として残す |
| 挿入／削除 | 確定した対応の中で相手側の空の区間を裏付けられ、必要な探索が完了し、欠落部分に対応内容が隠れる可能性が残らない | 候補または未解決。未対応であることだけでは確定しない |
| move | 内容の完全一致、出現箇所の一意な対応、移動を示す確定した前後関係 | move候補。反復する文や読み順推定の変更だけでは確定しない |
| 位置未確定の変更領域 | 有効な領域の組に対してtokenの多重集合が異なる、または片側だけに内容が存在することを裏付けられる | 任意に組にした領域を、変更が証明された領域とは扱わない |

tokenの多重集合が等しくても、順序や編集が同じとは限らない。
多重集合の不一致は、有効な対応領域の内容が異なる根拠になるが、置換位置や変更種類までは特定しない。
この違いを`proven_changed_regions`で保持する。

候補同士の重複は競合グループとして保持し、確定した範囲は重複して所有しない。
複数の出現箇所を一つの変更にまとめる場合は、それぞれを判定し、未確定の出現箇所を候補へ分ける。
確定した一箇所を根拠に、残りの出現箇所まで確定しない。
親の変更領域と、その内部の位置を推測する候補は共存できるが、別々のexactな変更として二重計上しない。

### 16.6 構造推定が外れた場合の回復

glyph、line、block、region、trusted run、source mapを使い、構造化を可逆なviewとして扱う。
推定した行や段落の境界はsoft境界として保持し、抽出の欠落や未対応内容の境界はhard境界として区別する。
すべてのlayout候補を文書全体について展開する仕組みは導入しない。

未解決の領域では、既存のblock view、原文が連続するline/run view、既存の境界に依存しないanchor recoveryの順に、有界かつ決定的に試す。
blockを分割または結合するviewは、tokenの所有範囲とseparatorの由来を保持する。
geometryは局所的な文字の相対値を使い、既知の文書名、producer名、page番号、特定の文言で処理を分岐しない。

複数の妥当なviewが異なる編集を示す場合は候補として残す。
採用するviewにかかわらず、確定には同じ領域の条件を適用する。
合成spaceや推定した段落境界だけの違いを内容変更にする場合も、原文の文字や空白配置に基づく根拠を必要とする。

回復の目的は、改行、改ページ、誤ったblock分割をまたいで正しい対応を増やすことである。
予算内で候補探索を広げ、他の領域が未解決でも独立した局所領域を確定できるようにする。
候補表示だけを改善して、確定できる変更や範囲が増えない状態を完成とはしない。

### 16.7 結果の型とcoverageの集計

既存の範囲型と有界なvectorを使い、原文は一度保持して側ごとのIDで参照する。
共有lockやglobalな可変状態は追加しない。

| 概念 | 条件 |
| --- | --- |
| `Comparison.changes` | 位置と種類を確定した内容変更だけを保持する |
| `Comparison.change_candidates` | 推測した編集と種類、原文範囲、競合グループID、未確定理由、必要なら順位を保持する。保持件数の打ち切りも明示する |
| `Comparison.proven_changed_regions` | 内容の不一致は確定しているが、変更位置は未解決の領域 |
| `Comparison.unresolved_regions` | 確定した同一範囲と変更範囲以外の内容。候補がある範囲を含み、理由と候補グループを参照する |
| 解決範囲のpartition | 各側の抽出済みtokenを、同一、変更、未解決に重複なく分ける |
| 抽出状態 | 未対応、未解決、資源制限を範囲付きで保持する。抽出できなかった量は不明として扱う |
| 比較の状態 | 変更あり、内容変更なし、判定不能を表し、抽出の完全性、変更位置の解決状況、比較対象の宣言を別に持つ |

確定状態を構築する内部処理はprivateまたは検証付きとする。
公開APIの変更ではconstructor、struct literalを使う呼び出し側、validator、serializerを同時に更新する。
利用側が結果を直接構築できるAPIを残す場合、その根拠はcaller assertionであることを明記し、構造上の整合を検証する。
report生成だけでは元のPDFを独立に再検証できない。

各側で次の関係を満たす。

```text
抽出済みcomparable token数
  = 同一と確定したtoken数
  + 変更と確定したtoken数
  + 未解決token数

解決済みcoverage
  = (同一と確定したtoken数 + 変更と確定したtoken数)
    / 抽出済みcomparable token数
```

件数はevent spanの単純合計ではなく、token区間の和集合から求める。
確定した挿入と削除は内容がある側を解決済みにし、零幅の境界はtoken数を消費しない。
候補と位置未確定の変更領域は、位置が確定するまで分子に含めない。
Formatting-onlyは内容の解決状態と直交し、独自にcoverageを増やさない。

抽出済みtokenのcoverageが100%でも、抽出が完全とは限らない。
欠落の範囲を別に表示し、分母が0なら百分率を表示しない。
画像だけのPDFでテキストが0件でも、ページ内容が同じという結論は出さない。
対応テキストを処理し終えた場合も、画像などの対象外内容は比較していないことを明示する。

部分的な抽出と、局所的に確定した差分は共存できる。
位置が不明な抽出問題は、その未知の内容に閉包が依存する領域を未確定にする。
位置が特定できる問題は、独立性を裏付けられる別領域まで無条件に失敗させない。
外部font identityはcallerの前提として残し、passwordや私的なassertion文字列はreportへ露出させない。

### 16.8 数値設定と資源制限

| 数値の種類 | 扱い |
| --- | --- |
| byte数、深さ、operator数、token数、出力範囲数などの資源上限 | parser、抽出、比較の境界で明示的に課金する。段階、資源名、上限、回復可能な範囲を記録する |
| 候補数、DP cell数、anchor window数などの探索上限 | 到達した場合は必要な探索の不完全性を残す。打ち切りの中で得た一位を一意な対応とは扱わない |
| 類似度、gap cost、spacing ratio、layout ratioなどのheuristic | 候補生成と順位付けに使う。役割、既定値、測定根拠、設定変更への感度を記録する |
| 範囲の妥当性、exact token一致、排他的所有などの不変条件 | 直接検証する。不正な設定と難しい入力を区別する |

fallback全体で共有する計算量と出力の予算を設け、各段階の消費を記録する。
viewを切り替えるたびに予算が無制限に復活する実装は認めない。
同点時の選び方と候補の順番を決定的にし、再実行で比較できる状態を保つ。
既存の既定値を出発点とし、開発用データの測定で必要性が確認できた場合に変更する。
適用したoptions、判定policyのversion、source revisionを評価結果へ残す。

局所的に安全に中断できる境界では、上限到達を未解決範囲として返し、独立した確定結果を維持する。
原文を安全に表現できない場合は、文脈を保持したfatalまたはlimitの結果を返す。
部分reportを作るために無制限に確保したり、空の成功結果に置き換えたりしない。
reportの書き込み失敗は実行エラーである。
この制限の存在から、すべての入力でpanicやallocation失敗が起きないと主張しない。

### 16.9 出力と互換性の移行

text reportには、確定した変更、位置未確定の変更領域、候補、未解決範囲を区別して表示する。
候補には`TENTATIVE`と理由を付け、色や`Low`だけに区別を任せない。
確定変更が0件でも、候補件数、未解決範囲、完全性を要約に残す。
出力上限やquiet modeによって候補の表示を省略しても、比較が完全になったとは扱わない。

JSONは次のschema versionで`changes`を確定結果に限定し、`change_candidates`、競合グループへの参照、構造化した判定理由、比較対象、差分の有無と完全性を追加する。
候補の順位を正解確率として出力しない。
report利用側、benchmarkへの結果変換、fixtureを同時に移行する。
benchmarkのsummaryにも独立したversionがあるため、その互換性も確認する。
旧形式への変換で候補を確定結果へ混ぜない。

差分の有無は、確定した変更または変更領域があれば「変更あり」、それらがなく対象テキストを完全に比較できれば「内容変更なし」、それ以外を「判定不能」とする。
「変更あり」と「比較不完全」は同時に成立する。
終了コードは§5.4に従い、比較不完全を既定で3とする。
独立した差分の有無が必要なscriptはJSONを参照する。
coreからprocess exit codeのpolicyをCLIへ移し、workspace内の呼び出しを移行する。

### 16.10 未使用文書による評価

#### 既存の検証資産と限界

`crates/pdfdelta-bench/tests/bench_matrix.rs`は、24ケースを二つのrendererで評価する48件の固定回帰matrixを持つ。
五つの初期受入条件を含め、このmatrixを維持する。
rendererは`lopdf-tj`と`classic-xref-tj`であり、二つの描画経路だけでproducerの多様性を検証したとはみなさない。
canonical generatorは小規模文書とASCIIに制限されているため、日本語や他のproducerの検証には既存の外部Typst fixtureなどを使う。
generatorの制限を広げる場合も、具体的なfixtureの必要性に基づく。

`crates/pdfdelta-bench/src/revisions.rs`は、完全annotationのscopeと部分annotationを分け、抽出が不完全な場合は品質評価を省略する。
この区別を維持しつつ、省略されたpairを実行結果の分母から消さない。
失敗文書を除外した後のprecisionだけで頑健性を評価しない。
`candidate_eval.rs`のexhaustive oracleとrecall@Kを再利用し、探索量の推定値と実際のlimit到達を区別する。

#### データの分割と来歴

調整前にmanifestを固定し、ファイルhash、改訂pair ID、取得元、文書系列ID、判明しているproducer系列とversion、annotation範囲、split、最初の評価日を記録する。
同じ文書の改訂版、抜粋、翻訳、template派生、生成したmutationは同じsplitへ置く。
byte単位の重複だけでなく、判明している内容とtemplateの派生関係も確認する。
producerが不明な場合は不明と記録し、未知producerの検証例とは数えない。

開発用データで既定値とアルゴリズムを調整し、文書系列ごとのholdoutを評価に残す。
独立したproducerが十分にある場合はproducer系列も分離したholdoutを設け、二つの未知性を分けて報告する。
既存文書をすでに修正に使っている場合、新しいholdoutとして再分類しない。
失敗を見て実装を調整したグループは開発用へ移し、その後の評価には未使用の文書を用意する。
失敗した評価も保存し、同じ評価データへの反復調整を追跡できるようにする。

#### Annotationと照合

正解には、原文範囲、変更種類、繰り返しの出現箇所、原文自体が許す複数解、変更のない周辺範囲、annotationの境界を記録する。
現在の出力を正解に合わせる形でannotationを作らない。
文書全体を注釈できない場合は、完全に注釈した限定scopeを使い、評価範囲を明示する。

eventの照合はold/newを区別した一対一対応とする。
繰り返す引用には位置または出現IDを付け、挿入と削除は内容がある側の範囲と相手側の零幅境界を使う。
既存のoverlap規則を比較前に固定し、新しい出力を正解にするために許容値を動かさない。
overlapによる照合に加えて、原文範囲の完全一致率も報告する。

#### 評価指標

| 対象 | 指標と集計規則 |
| --- | --- |
| 位置まで確定した変更 | 完全annotation範囲でprecision、recall、範囲の完全一致率、種類の正解率、断片化、未変更tokenの誤検出率を測る。未解決に残った正解変更も見逃しに数える |
| 差分候補 | 候補recall、完全annotation範囲の候補precision、競合グループ当たりの候補数、確認対象event/token数を測る。同じ正解に複数候補が一致してもrecallは一件とし、余分な誤候補は隠さない |
| 位置未確定の変更領域 | 変更の存在を示す主張の正しさと、位置未確定の割合を測る。exactな置換や挿入の正解件数へ加えない |
| 解決状況 | 各側の同一、変更、未解決のtoken和集合、抽出完全性、量が不明な欠落、文書全体の比較完了率を報告する |
| 実行結果 | 品質評価を省略したpair、未対応、limit、fatalを含む全試行と、環境付きの時間およびmemoryの分布を報告する |

部分annotationは、列挙した変更のrecallと種類を評価する用途に限定する。
precisionの分母や文書全体のrecallには使わない。
長い文書だけが集計を支配しないよう、文書単位と系列単位の結果を総数と併記する。
推定の区間を報告する場合は独立した文書系列を標本の単位とし、token数を独立標本数にしない。
confidenceを確率として解釈するには、別の未使用データによる校正を必要とする。

比較の基準は二つ保存する。
一つは現行production出力で、そのconfidenceとcoverageの定義を明記する。
もう一つは、共通判定だけを導入し、recoveryを改善する前の出力である。
前者で利用上の検出低下を確認し、後者で候補への降格だけを回復性能の改善と数えることを防ぐ。
新しい確定結果だけのprecisionを、旧形式の混在したprecisionと単独比較しない。
recall、候補の確認量、coverage、同じ分類を適用した基準値も示す。

#### 検証matrix

| fixture | 必要な確認 |
| --- | --- |
| 五つの初期受入条件 | 改行と改ページは内容変更0件かつ比較完全。置換、段落挿入、段落削除は、正しい範囲と種類の確定変更が各一件。候補への移動では合格にしない |
| 内容を保つ変換 | font size、margin、page size、line height、改行、改ページ、同じ描画を行うoperator/object構成を変えても内容結果を維持する。任意の描画順変更を同値とは仮定しない |
| 内容変更と表示変更の組み合わせ | 既知の意味的mutationに表示変更を重ねても正解を維持する。期待位置は文字列検索でなくmutationの来歴から求める |
| 境界推定の誤り | blockの分割と結合、interleaved rendering、反復header、pageをまたぐ文を回復する。未解決の理由も測るが、棄却できたことだけでrecallの失敗を合格にしない |
| 曖昧な対応 | 繰り返す条文、見出し、数字、競合anchor、複数の編集位置から、根拠のない種類や位置を確定しない。近くの曖昧でない変更は検出できることも確認する |
| 抽出境界 | Unmapped、font identity欠落、未対応stream、clipや非表示文字、画像だけの入力、復号できない暗号、局所的な欠落を保持する。欠落を削除や同一へ変換しない |
| 資源境界 | 到達可能なbyte、展開量、深さ、glyph/token、候補、DP、fallback、出力上限の直前と境界と直後を試す。予算増加で競合が見つかるなら、少ない予算の時点で探索完了を主張しない |
| 結果の不変条件 | partitionの完全性、確定範囲の排他性、候補の重複、複数出現の判定、未確定の同一範囲、old/new反転、決定性、出力打ち切りを確認する。反転では表示順やIDでなく意味と原文範囲を比べる |
| CLIとreport | 差分の有無と完全性の全組み合わせ、空text、対象外画像との混在、quiet、strict alias、schema version、不正な直接構築結果、書き込み失敗を確認する |

engineの条件には`Document<Glyph>`を使い、parserと抽出の条件にはPDF fixtureを使う。
不正入力には既存の`parser_entry`、`content_stream_parser`、`cmap_parser`、`font_decoder`、`glyph_extraction`のfuzz targetを再利用する。
nightlyの例外はfuzz crateだけに維持する。
有限時間のfuzz実行からcrashしないこと全般を証明したとは扱わない。

開発用データでは、score、margin、layout設定、候補予算を既定値の周囲で変化させ、系列ごとの影響を調べる。
小規模fixtureの候補recallはexhaustive oracleと比較する。
確定結果が設定によって変わる場合は根拠を調べ、holdoutで最も良い値を選ぶ調整には使わない。
誤検出と未解決coverageの関係も報告し、一点の良い値だけで不安定さを隠さない。
最終optionsはholdout評価の前に固定する。

#### 公開条件

五つのexactな受入条件、共通判定の不変条件、資源制限時の定義した結果を必須条件とする。
既存の回帰matrixと明示的な未対応caseの期待値を維持し、移行を通すために期待結果を弱めない。

固定した完全annotation付き開発データでは、共通判定導入後の基準に対して、recovery変更が確定結果の誤検出件数と未変更tokenの誤検出率を増やさず、確定変更のrecallと解決済みcoverageを維持することを求める。
そのうえで、事前に対象とした構造上の失敗の少なくとも一つについて、正しい確定変更または解決範囲が増えることを確認する。
候補だけの改善は別に報告し、この回復条件の達成には数えない。

holdoutでも同じ比較を行い、系列ごとの悪化をすべて記録する。
新たな誤確定は公開前に調査し、recallやcoverageの低下は原因と公開判断を明示する。
集計値の改善だけで個別の悪化を隠さない。
修正が必要になったholdoutは開発用へ移し、次の汎化評価には新しい文書を用意する。
現在の標本から普遍的な99%などの目標値を作らない。
合格が示すのは評価した系列に対する結果であり、すべてのPDFへの保証ではない。

### 16.11 実装順序と完了の証拠

| 段階 | 担当する層と作業 | 次へ進むための証拠 |
| --- | --- | --- |
| 1. 基準と契約 | core/benchで数値と確定経路を分類し、現行出力とsplitを固定する。誤順序、反復文、抽出gap、探索打ち切りのglyph fixtureを用意する | caseごとの失敗、候補と確定の集計期待値、五つの受入条件を保持した再現可能な基準 |
| 2. 共通判定 | coreで対応判定と最終所有範囲を導入し、通常経路と直接比較APIへ接続する | confidenceだけで昇格しないこと。候補の変更と同一範囲がcoverageを増やさず、不正な根拠を確定出力へ渡さないこと |
| 3. 構造の回復 | 既存recoveryを共通判定へ接続し、実例で必要な可逆viewと共有予算を追加する | 境界変更の回復、競合の保持、決定性、打ち切りの可視化、対象とした失敗での正しい検出または解決範囲の増加 |
| 4. 公開出力 | core report、CLI、benchで候補、次のJSON schema、原文表示、完全性、終了コードを移行する | 状態の組み合わせ、候補と親領域の重複、競合グループ、出力上限、書き込み失敗の統合テスト |
| 5. 汎化評価 | 固定した評価手順と設定感度の確認を実行し、系列ごとの結果と既知の失敗を保存する | 五つの受入条件と契約テストの通過、split混入や分母変更のない評価、誤確定、見逃し、候補、未解決の全指標 |

各段階を依存順の小さな変更として実装する。
parserの作業でengine開発が止まらないよう、programmaticな`Document<Glyph>` fixtureを維持する。
最初の二段階だけを機能の完成とせず、回復と評価までを未知PDFへの対応改善に含める。
新しい表認識やOCRへ進む前に、この設計の対応範囲で根拠と結果を揃える。

実装commit前には次を実行する。

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

benchmark matrixまたは抽出適合性を変更した場合は、該当する`cargo test -p pdfdelta-bench --test bench_matrix`または`cargo test -p pdfdelta-bench --test extraction_conformance`で対象の条件を確認する。
実文書の評価には、実行command、manifest、annotation hash、source revision、options、raw resultを残す。
外部入力を取得できなかった実行は、合格ではなく実行不能として記録する。
この設計の追加だけでは実装や検証の完了を主張しない。
