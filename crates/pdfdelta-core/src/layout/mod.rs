mod block;
mod geometry;
mod line;
mod region;

pub(crate) use block::validate_block_options;
pub use block::{Block, BlockId, BlockOptions, BlockRole, reconstruct_blocks};
pub(crate) use line::validate_line_options;
pub use line::{Line, LineId, LineOptions, LineTextDirection, SyntheticSpace, reconstruct_lines};
pub use region::{
    ReadingOrder, Region, RegionGraph, RegionId, RegionOptions, RegionRelation, partition_regions,
    validate_region_options,
};
