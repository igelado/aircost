//! Listing-only aircraft valuation.
//!
//! Database code is deliberately confined to [`dataset`] and [`store`]. The
//! comparable and structural estimators operate on frozen, plain Rust values.

pub mod comparable;
pub mod dataset;
#[cfg(feature = "dnn")]
pub mod dnn;
pub mod store;
pub mod structural;
pub mod types;
pub mod validation;

pub use comparable::{ComparableConfig, ComparableModel};
pub use structural::{fit_structural, StructuralFitConfig, StructuralModel};
pub use types::*;

pub const FEATURE_SCHEMA_VERSION: u32 = 1;

pub trait ValuationModel: Send + Sync {
    fn model_version_id(&self) -> i64;
    fn model_kind(&self) -> &'static str;
    fn snapshot_id(&self) -> i64;
    fn estimate(&self, query: &ValuationQuery) -> Result<ValuationEstimate, ValuationError>;
}
