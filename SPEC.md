# pdfdelta 技術仕様書 v3

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
| xref stream / object stream を含む現代的なPDF | 暗号化PDF、PDF 2.0固有機能の網羅 |

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
}

pub trait ParsedPdf: Send + Sync {
    fn version(&self) -> PdfVersion;
    fn trailer(&self) -> Result<PdfDict>;
    fn resolve(&self, reference: ObjectRef) -> Result<PdfObject>;
    fn pages(&self) -> Result<Vec<PageRef>>;
    fn page_dict(&self, page: PageRef) -> Result<PdfDict>;
    fn raw_stream(&self, reference: ObjectRef) -> Result<RawStream>;
    fn decoded_stream(&self, reference: ObjectRef) -> Result<DecodedStream>;
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

**自前実装するもの**：parser library adapterと中立facade、Content Stream / Text State解釈、ToUnicode/CMapの必要部分、Glyph geometry復元、Glyph→Line→Block→Regionの構造化、Normalization、Anchor検出、candidate generation、Alignment(1:N/N:1、move検出、confidence/coverage算出)、Myers Diff。

**production dependencyとして使ってよいもの**：A0で選定した既存PDF object parser backend(初期候補`lopdf`)、CLI parsing(`clap`)、JSON(`serde`/`serde_json`)、logging、error整形、必要なstream decode。圧縮アルゴリズムや汎用PDF object parserの再実装は初期目的から外れる。

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

`TextSpan`はBlock集合、複数Blockを結合した際の`BlockSeparator`、canonical文字範囲、comparable token範囲を保持する。単一Blockではseparatorを持たず、複数Blockでは`Concatenate`または`Space`を必須とする。これにより同じBlock集合でも結合方法によって変わる範囲indexを一意に解釈できる。

変更の単位は「対応付いたBlock集合の上のcanonical文字範囲」とする。範囲indexは§8のUnicode scalar indexであり、`MappedText.source_map`を通じてGlyph、page、geometryへ逆写像できる。
「1 replacement」とは、連続するcanonical文字範囲の置換1件を指す。

### 5.2 Confidence と Coverage

結果には必ず、個々の判定の確からしさと、比較できた範囲を分けて持たせる。

- **Confidence**：個々の変更判定をどれだけ信用できるか。
- **Extraction completeness**：全pageのContent Streamと、そこから到達するtext-bearing Form XObjectをparser backendとPrimitive Extractionが処理できたか。画像XObjectはscope外として明示的にskipできるが、種別不明または未解釈のstreamを「文字数0」としてcoverageの分母から消してはいけない。
- **Old alignment coverage**：old側でalignmentが解決したcomparable token数 / old側で抽出できたcomparable token総数。
- **New alignment coverage**：new側でalignmentが解決したcomparable token数 / new側で抽出できたcomparable token総数。

`comparable token`にはcanonical Unicode code pointと、§6.4の`Unmapped` tokenを含む。十分なconfidenceで1:0 deletionまたは0:1 insertionと分類できたtokenも「解決済み」としてcoverageへ含め、変更が多い文書ほどcoverageが下がる定義にはしない。CLIの`Comparison coverage`は`min(old alignment coverage, new alignment coverage)`を表示するが、JSONにはold/newを別々に保持する。Extractionが不完全な場合は数値だけで完全性を装わず、`extraction_complete: false`と未解釈regionを必ず併記する。ページ面積比は表示上の参考値にとどめる。

### 5.3 出力カテゴリ

canonical正規化(§8)で吸収した差も黙って消さず、独立したカテゴリで報告する。canonical textが同一のaligned spanについて、line/page break、Block分割、font size、position等の差を確実に識別できた場合もFormatting-onlyへ含める。このカテゴリはbest-effortであり、0件でもrenderingが完全同一であることは保証しない。exit codeのContent change判定には影響させない。

```text
Content changes:          1
Formatting-only changes:  3    ← canonicalで吸収された差(改行位置の変化など)
Uncertain changes:        1
Unresolved regions:       2
Extraction complete:      old=yes, new=yes
Alignment coverage:       old=97.8%, new=97.4%
Comparison coverage:      97.4%
```

「No differences found」とだけ表示することはない。

### 5.4 CLI と exit code

初期CLIは次の三つとする。CLI parserには`clap`を使う。

```bash
pdfdelta inspect document.pdf
pdfdelta old.pdf new.pdf
pdfdelta old.pdf new.pdf --json result.json
```

`inspect`にはbackend確認用の`--backend-info`、object確認用の`--objects`、Glyph確認用の`--glyphs`を段階的に追加する。

exit codeはCI利用を前提に定義する。

| code | 意味 |
|---|---|
| 0 | Content changeなし。`--strict`時はcomparisonもcomplete |
| 1 | Content changeあり |
| 2 | 実行不能なerror(I/O、fatal parse error等) |
| 3 | `--strict`時にUNSUPPORTED / UNRESOLVEDがあり、comparisonが不完全 |

判定優先順位は`2 > 3 > 1 > 0`とする。既定ではUNSUPPORTED / UNRESOLVEDをstderrとreportへ出し、既知のContent change有無に応じて0または1を返す。`--strict`では「差分あり」と「比較不完全」を混同せず3を返す。

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
- Page Tree走査とResourcesの継承。
- raw stream metadataとdecoded stream bytesの取得。
- 初期必須filterとしてFlateDecode。未対応filterは空文字へ潰さずUNSUPPORTEDにする。
- 暗号化の検出。初期scopeでは復号せずUNSUPPORTEDにする。
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

`q` / `Q` / `cm`でGraphics StateとCTMを追跡する。`Do`ではXObject subtypeを判定し、Form XObjectだけResourcesとMatrixをstackへ積んで再帰上限付きで解釈する。Image XObjectは画像内容比較のscope外として明示的にskipする。`'`と`"`は対応する複合operatorへ展開してから処理する。

Pageの`/Contents`が複数streamのarrayである場合は指定順に解釈し、stream境界でtokenが連結しないよう論理的なseparatorを置く。inline imageの`BI` / `ID` / `EI`は画像としてskipしつつ、後続operatorとの同期を失わない専用lexer処理を持つ。

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

Line境界は一律に空文字へ置換しない。同一Block内のsoft line breakについて、前後がLatin letter / digitなら原則single space、CJK同士なら原則empty、line-end hyphenationならhyphenとbreakを除去する。明示spaceが既にある場合は重複させない。判定が曖昧な場合はrawを保持してUNRESOLVEDまたは低confidenceのFormatting-onlyとし、単語を黙って連結しない。Paragraph境界はBlock境界でありcanonicalから除去しない。

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

### 9.2 Anchor の順序整合と move 候補

old→newのanchor対応列に対し、new側indexのLongest Increasing Subsequenceを取り、main chainとする。局所的に同じ文字列が現れても、文書全体の順序から不自然な対応を除外できる。

LISから外れたanchor(例：`3 → 8`)は捨てない。順序を乱す対応はまさにParagraphMoveの痕跡であるため、**move候補**として保持し、robust alignment段(§9.6)で再評価する。move候補が十分なscoreで裏付けられれば`ChangeKind::Move`として報告し、裏付けられなければ通常のdeletion+insertionまたはUNRESOLVEDに落とす。

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

B3では1:1、1:0、0:1に加え、同一anchor区間内で隣接Blockだけを結合する制約付き1:2 / 2:1を扱う。これは§2.2 Case 1/2で、line wrapやpage boundaryにより片方だけBlockが分割された場合に必要である。B4では1:3 / 3:1を追加し、より一般のDynamic Programmingへ拡張して構造化誤差を吸収する。

Matchingは一回で確定しない。`A X B` / `A Y B`のように前後(`A ↔ A`、`B ↔ B`)が確定した後に`X ↔ Y`のscoreを引き上げる、initial matching → neighbor consistency → refinementの二段階とする。

### 9.7 Candidate generatorの採用条件

小規模documentでは全Block pairをscoreするexhaustive実装をtest oracleとして保持する。inverted indexまたはLSHは、true alignmentがcandidate上位K件に残るrecall、候補件数、latency、memoryをこのoracleと比較する。

LSHは、holdoutを含むbenchmarkでcandidate recallを悪化させず、候補件数または実行時間を明確に改善した場合にだけdefault化する。改善がなければtrait実装は残してもdefaultはinverted indexのままとする。

---

## 10. Exact Diff とその先

### 10.1 Myers Diff

Alignment完了後だけ実行する。Myers algorithmを自前実装し、最初はcharacter-level、その後 token-level → changed token only → character-level に拡張する。

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

閾値と重みのチューニングは合成benchmarkで行い、人手review済みのreal-world pairはholdoutとして評価にのみ使う。tuning用とevaluation用のデータを混ぜない。

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
