mod block;
mod geometry;
mod line;

pub use block::{Block, BlockId, BlockOptions, BlockRole, reconstruct_blocks};
pub use line::{Line, LineId, LineOptions, SyntheticSpace, reconstruct_lines};
