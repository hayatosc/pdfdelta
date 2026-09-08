mod block;
mod geometry;
mod line;
mod region;

pub use block::{Block, BlockId, BlockOptions, BlockRole, reconstruct_blocks};
pub(crate) use block::{
    LayoutIssue, TrustedRegionEdge, TrustedRunDescriptor, TrustedRunId, TrustedRunInterval,
    reconstruct_blocks_with_issues, validate_block_options,
};
pub(crate) use line::validate_line_options;
pub use line::{Line, LineId, LineOptions, LineTextDirection, SyntheticSpace, reconstruct_lines};
pub(crate) use region::UncertainLineReason;
pub use region::{
    ReadingOrder, Region, RegionGraph, RegionId, RegionOptions, RegionRelation, partition_regions,
    partition_regions_with_vector_lines, validate_region_options,
};
