mod block;
mod geometry;
mod line;

pub(crate) use block::validate_block_options;
pub use block::{Block, BlockId, BlockOptions, BlockRole, reconstruct_blocks};
pub(crate) use line::validate_line_options;
pub use line::{Line, LineId, LineOptions, SyntheticSpace, reconstruct_lines};
