//! The Gen 2 memory model (ADR 0001).
//!
//! Five concerns are kept as separate data (D2):
//! - **virtual addressing**: title address → page-table entry, filled when a
//!   region is mapped and invalidated when it is freed, with a generation
//!   tag so a stale reference is detected ([`space`]);
//! - **physical residency**: where a page's bytes live, VRAM or coherent
//!   system memory ([`Home`]);
//! - **access contract**: who may read and write, declared or proven
//!   ([`contract`]);
//! - **ownership**: the current authority for the bytes ([`Ownership`]);
//! - **observed usage**: counters that may *propose* a contract change but
//!   never apply one (D3), reported through `sixth-telemetry`.
//!
//! Late CPU writes to GPU-owned pages follow D4 ([`merge`]), and CPU→GPU
//! forwarding is coalesced and budgeted (D5, [`forward`]).
//!
//! Nothing here touches a GPU: the crate decides, the backends act. Every
//! decision is a returned [`Action`] and a telemetry event.

pub mod contract;
pub mod forward;
pub mod merge;
pub mod ranges;
pub mod registry;
pub mod space;
pub mod timeline;

pub use contract::{Contract, Declared, Home, Ownership};
pub use forward::{CostModel, Plan, Planner, Run};
pub use space::{Action, AddressSpace, MapError, PageEntry, PageRef, RegionId};
pub use ranges::RangeSet;
pub use registry::{Registry, ResourceId, Version};
pub use timeline::{CpuPlan, Hazard, Retired, SubmitPlan, Timeline};

/// A page: the unit of the page table, of residency and of ownership.
pub const PAGE: u64 = 64 * 1024;
/// The unit the OS reports CPU writes in (write-watch, guard pages), and so
/// the unit CPU→GPU forwarding copies.
pub const UNIT: u64 = 4 * 1024;
/// Dirty units per page.
pub const UNITS_PER_PAGE: u32 = (PAGE / UNIT) as u32;
