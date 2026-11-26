//! Dependency-aware scheduling (ADR 0001 D7, ADR 0002 §4.4).
//!
//! The inherited submission path records the title's commands, guesses
//! resource states per buffer (`BufferStates`), and waits for the whole
//! submission before the next one is prepared. This crate is the
//! replacement's core, kept free of D3D12 so that every rule is a unit
//! test:
//!
//! - [`hazard`]: from each command's declared uses (resource, subresources
//!   or bytes, access), the barriers it needs and the earlier commands it
//!   depends on, with read-after-write, write-after-write and
//!   write-after-read told apart, at byte and subresource granularity;
//! - [`batch`]: uploads gathered into as few copies as the dependencies
//!   allow, each placed before its first reader rather than all at the top.
//!
//! Retirement by fence and the CPU side of the same rules are in
//! `sixth_memory::timeline`.

pub mod batch;
pub mod hazard;

pub use batch::{Upload, plan_uploads};
pub use hazard::{Access, Barrier, Tracker, Use};
