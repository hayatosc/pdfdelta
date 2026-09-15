//! Equality requires exact acquired dependencies, not their hashes or a raster.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PROFILE: &str = "opaque-invoked-resources-v2";

/// Object references have been expanded with checked depth/work limits. Stream
/// contents are retained exactly; their dictionary and decoded bytes both matter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Value {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    Name(Vec<u8>),
    String(Vec<u8>),
    Array(Vec<Value>),
    Dictionary(BTreeMap<String, Value>),
    Stream {
        dictionary: BTreeMap<String, Value>,
        bytes: Vec<u8>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Operation {
    pub operator: String,
    pub operands: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Closure {
    pub profile: String,
    pub backend: String,
    /// Equal entry state is part of the claim, including the relevant backdrop.
    pub entry: Entry,
    pub page: BTreeMap<String, Value>,
    pub resources: Value,
    pub commands: Vec<Operation>,
    /// Preserve lexical operands too: the syntax adapter's f32 values are not
    /// an injective representation of PDF decimal tokens.
    pub command_bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entry {
    pub graphics_state: String,
    pub backdrop_rgb: [f64; 3],
    pub optional_content: String,
    pub external_overlaps: String,
    pub output_footprint: String,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            graphics_state:
                "PDF initial graphics state; page transform and clip from page dictionary".into(),
            backdrop_rgb: [1.0; 3],
            optional_content:
                "no optional-content execution; catalog and page visibility extensions rejected"
                    .into(),
            external_overlaps: "complete page program; annotations and external paint unsupported"
                .into(),
            output_footprint: "page media/crop intersection, with declared rotation and user unit"
                .into(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Issue {
    pub dependency: String,
    pub location: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Page {
    pub input_sha256: String,
    pub page: usize,
    pub source_object: pdfdelta_core::pdf::ObjectRef,
    pub sources: Vec<pdfdelta_core::pdf::ObjectRef>,
    pub issues: Vec<Issue>,
    pub expanded_bytes: usize,
    pub visits: usize,
    /// Private acquisition result. Serialized observations cannot be reloaded
    /// as trusted certificates; the source acquisition must run again.
    #[serde(skip)]
    pub closure: Option<Closure>,
    pub closure_sha256: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Comparison {
    Equivalent {
        profile: String,
        checked_dependencies: Vec<String>,
    },
    Unresolved {
        reason: String,
    },
    /// Dependency inequality only rejects this sufficient equality proof. It
    /// does not prove different pixels, text or an author's intended change.
    Rejected {
        changed_dependencies: Vec<String>,
    },
}

pub fn compare(old: &Page, new: &Page) -> Comparison {
    let (Some(a), Some(b)) = (&old.closure, &new.closure) else {
        return Comparison::Unresolved {
            reason: "one or both dependency closures were not acquired under the profile".into(),
        };
    };
    if !old.issues.is_empty()
        || !new.issues.is_empty()
        || a.profile != PROFILE
        || b.profile != PROFILE
        || a.backend != b.backend
    {
        return Comparison::Unresolved {
            reason: "acquisition, profile or backend identity is incompatible".into(),
        };
    }
    let dependencies = [
        (
            "entry_graphics_state",
            a.entry.graphics_state == b.entry.graphics_state,
        ),
        ("backdrop", a.entry.backdrop_rgb == b.entry.backdrop_rgb),
        (
            "optional_content",
            a.entry.optional_content == b.entry.optional_content,
        ),
        (
            "external_overlaps",
            a.entry.external_overlaps == b.entry.external_overlaps,
        ),
        (
            "output_footprint",
            a.entry.output_footprint == b.entry.output_footprint && a.page == b.page,
        ),
        (
            "resolved_resources_masks_opacity_blend_and_nested_forms",
            a.resources == b.resources,
        ),
        (
            "commands_operands_transform_clip_and_invocation_context",
            a.commands == b.commands && a.command_bytes == b.command_bytes,
        ),
    ];
    let changed: Vec<_> = dependencies
        .iter()
        .filter_map(|(name, equal)| (!equal).then_some((*name).to_owned()))
        .collect();
    if changed.is_empty() {
        Comparison::Equivalent {
            profile: PROFILE.into(),
            checked_dependencies: dependencies
                .iter()
                .map(|(name, _)| (*name).to_owned())
                .collect(),
        }
    } else {
        Comparison::Rejected {
            changed_dependencies: changed,
        }
    }
}
