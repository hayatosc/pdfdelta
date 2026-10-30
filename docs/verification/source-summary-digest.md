# C63: source-summary digest bufferingの検証記録

保存対象は `5b2d2f5da9455e652bf47bd8b39934af47dd0aec`、比較基準は親のC53 `355d03518f028c1267bfc8819be67a01ea4f36ba`。採用した効果は速度改善で、**実PDFのstrict完全処理率は1/36のまま、新しい確定情報は0**。mainへの統合を意味しない。後続の未採用C70・C75・C77はこのスナップショットに含まれない。

`JsonSourceSummary::new`でsource配列をSHA-256へ直列化するとき、8KiBの`BufWriter`で小さなwriteをまとめ、flush成功後にdigestを確定する。巨大な配列全体をbufferには保持しない。schema、CLI flag、依存関係、抽出・比較予算、証明条件は変更していない。通常compact APIと`--native-text-only --json`が対象。

## 固定データの保持

2026-10-30の採用時検証では、同じ公式36組・72入力SHA-256、同じflags、抽出5,000,000・比較32,000,000の条件で再実行した。**36/36のgzip bytes、終了コード、compact summaryがC53と一致**。新しい36出力を独立に再hashし、65,589,403bytesの保持を確認した。source配列のSHA・byte数、公開range・条件付き情報・statusに損失はない。独立したPDF抽出器によるraw glyph再生成の検証とは区別する。

内訳は完全処理1、抽出不完了9、抽出完了かつ比較上限到達19、上限未満の未完了7。空抽出4は成功に含めない。

## 性能

**348 CLI実行＝本測定336＋warmup12**。基準2本・候補2本の同長pathを各blockで均衡順に実行した。5ケースは12blocks、Schedule SEは24blocks。全回のgzip/logical SHA・終了コードを確認し、348記録と集計を独立監査した。外れ値除外、追加追試、ケース入替えはない。

比はblock内の候補2回の和／基準2回の和の中央値。片側中央値上限に6ケース×2指標へのalpha=0.05/12配分を用いた。Controls/IPCCは点推定・上限とも0.95以下、他4件は1.05以下を要求し通過した。全A/A・B/B中央値も0.95–1.05内。

|対象|実時間短縮|実時間比の上限|CPU短縮|CPU比の上限|
|---|---:|---:|---:|---:|
|Controls|17.01%|0.847344|14.16%|0.885963|
|IPCC|16.07%|0.849134|12.95%|0.890766|
|GPT-3|14.67%|0.882302|12.88%|0.905528|
|Mask R-CNN|13.73%|0.891919|12.18%|0.899342|
|DDPM|8.59%|0.932070|8.09%|0.944290|
|Schedule SE|14.01%|0.897444|12.77%|0.902295|

固定build・共有hostの結果で、区間はblock比の独立同分布などを仮定する。全環境への速度保証ではない。最大RSS比は1.036674。aggregate 6,000,000,000bytes・swap0、jobs1・test threads1。計測時peak5,999,996,928bytes、maxイベント2037、OOM/OOM kill0であり、メモリに余裕があるとの主張はしない。

## 品質と再現

採用コミット前の6ゲートは全通過した。

|ゲート|結果|
|---|---|
|fmt / all-targets・all-features clippy|exit0、warningsをerrorとして検査|
|workspace tests|3575 passed、0 failed、2 ignored|
|all-features library tests|2269 passed、0 failed、2 ignored|
|docs|warningsをerrorとしてexit0|
|bench verify|policy48/48、strict author-intent42/48|

benchの42/48と実PDFの1/36は別の指標。回帰testはpartial write、WriteZero、flush失敗、byte数overflow、SHA block／8KiB境界、小write集約、独立Pythonによる18例のdigest/field集計照合を含む。

Rust1.98.0、Edition2024、Cargo.lockを固定。隔離した6GB/swap0の環境で次を実行する。既存baselineや原本PDFを上書きしない。

```sh
cargo build --locked --release -p pdfdelta-cli
target/release/pdfdelta OLD.pdf NEW.pdf --native-text-only --limit-scale 1 --quiet --json NEW_OUTPUT.json.gz
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --workspace --all-features --lib
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --document-private-items
cargo run -p pdfdelta-bench -- verify
```

凍結した72入力のhash、348測定のraw記録、事前計画、実行・独立監査脚本、全ログは以下のローカルcheckpointに保存した。この記録はcheckpoint自体をGitHubにアップロードしたとの主張ではなく、raw corpus・認証情報・会話履歴も同梱しない。

|保存対象|SHA-256|
|---|---|
|`checkpoint-c63-accepted-complete.zip`（94,227,572bytes、manifest514項目、source484ファイル独立復元）|`f159908225f65bf9bc251a2769bcba8115015e451860256ea0dea8bb776bb026`|
|source ZIP、484 files・lockfile・oracle fixture|`982e8a505499f76658bc3ea1c89f0e3f40c1864cddd236dae75d4937da3a4ef3`|
|C63 release binary|`de5dbd454d38cb3634ef2afefb30187b2b6bc724c31b93c3a885ec753e106457`|
|性能raw記録|`eed5a93e1fafbc9b23a049aace4e4034ec62e57e31def9412e2a2c040b36cb63`|

Git履歴は既存祖先`7cf0bae7689506065421ab82446011ebf5f078cf`からC63までの14コミットを元のSHAで保存した。この範囲には当時は候補として記録された祖先もあり、過去の各コミットを個別に採用済みと再分類するものではない。ここで検証済みとして固定するのは、上記の条件を通過した最終C63ツリー`d3b0739072a137c397cc373556fb3fe55fe98f17`である。
