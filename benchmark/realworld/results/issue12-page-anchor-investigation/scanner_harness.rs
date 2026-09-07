#[path = "../crates/pdfdelta-core/src/diff/recovery/page_anchor.rs"]
mod page_anchor;
use page_anchor::*;
struct Block { id: u64, eligible: bool, role: u32, pages: Vec<u32>, tokens: Vec<char> }
fn read(path: &str) -> Vec<Block> {
 let data = std::fs::read(path).unwrap();
 let mut cursor = 0; let mut blocks = Vec::new();
 while cursor < data.len() {
  let end = cursor + data[cursor..].iter().position(|b| *b == b'\n').unwrap();
  let h: Vec<_> = std::str::from_utf8(&data[cursor..end]).unwrap().split_whitespace().collect();
  let len: usize = h[4].parse().unwrap();
  cursor=end+1;
  let tokens=std::str::from_utf8(&data[cursor..cursor+len]).unwrap().chars().collect();
  assert_eq!(data[cursor+len], b'\n'); cursor+=len+1;
  blocks.push(Block{id:h[0].parse().unwrap(),eligible:h[1]=="1",role:h[2].parse().unwrap(),pages:h[3].split(',').map(|s|s.parse().unwrap()).collect(),tokens});
 }
 blocks
}
fn candidates(blocks: &[Block]) -> Vec<PageAnchorCandidate<'_, char, u32>> {
 blocks.iter().map(|b| PageAnchorCandidate::new(b.id,b.role,Some(&b.pages),&b.tokens,b.eligible)).collect()
}
fn main() {
 let old=read("target/issue12-page-anchor-old.txt"); let new=read("target/issue12-page-anchor-new.txt");
 let limits=PageAnchorSelectorLimits{max_blocks_per_side:2000,max_scanned_windows_per_side:1000000,max_scanned_token_work:4 * old.iter().chain(new.iter()).map(|b| b.tokens.len()).sum::<usize>(),max_retained_windows_per_side:1000000,max_pairs:10000};
 let anchors=select_page_anchors(&candidates(&old),&candidates(&new),128,limits).unwrap();
 println!("{anchors:#?}");
 assert_eq!(anchors.len(),2);
 assert_eq!((anchors[0].old.block_id, anchors[0].new.block_id),(293,519));
 assert_eq!((anchors[1].old.block_id, anchors[1].new.block_id),(298,538));
 let mut bounded=limits; bounded.max_scanned_token_work=1;
 assert_eq!(select_page_anchors(&candidates(&old),&candidates(&new),128,bounded),Err(PageAnchorSelectorError::Limit(PageAnchorSelectorResource::TokenWork)));
}
