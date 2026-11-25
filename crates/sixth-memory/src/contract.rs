//! Access contracts (D3) and what follows from them: the initial ownership
//! and the home (D1).
//!
//! Ownership comes from a contract, never from watching a few frames:
//! - **declared**: API semantics, console memory-type bits, DirectStorage
//!   destinations, explicit copies;
//! - **proven**: an analysis of an authorized or independently authored
//!   binary, recorded with its evidence;
//! - **default**: nothing is known, so the page behaves as `Mixed`.

/// Who currently has authority over a page's bytes (Part 3 §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ownership {
    /// Not classified: behaves exactly like `Mixed`.
    Unknown,
    /// Only the CPU writes; the GPU reads a forwarded copy.
    CpuOwned,
    /// Only the GPU writes; the CPU reads through declared readbacks.
    GpuOwned,
    /// Nobody has written since the last transition; both copies are equal.
    ReadShared,
    /// Both write. Lives in coherent memory. Sticky for the session.
    Mixed,
}

impl Ownership {
    pub fn name(self) -> &'static str {
        match self {
            Ownership::Unknown => "unknown",
            Ownership::CpuOwned => "cpu_owned",
            Ownership::GpuOwned => "gpu_owned",
            Ownership::ReadShared => "read_shared",
            Ownership::Mixed => "mixed",
        }
    }

    /// Unknown pages are handled as Mixed: the safe behaviour.
    pub fn behaves_as_mixed(self) -> bool {
        matches!(self, Ownership::Unknown | Ownership::Mixed)
    }
}

/// Where a page's bytes physically live (D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Home {
    /// Sparse VRAM: GPU-hot memory. The GPU reads at VRAM speed.
    Vram,
    /// Coherent system memory: the CPU reads it often, or both sides write.
    Coherent,
}

impl Home {
    pub fn name(self) -> &'static str {
        match self {
            Home::Vram => "vram",
            Home::Coherent => "coherent",
        }
    }
}

/// A declared contract: what an API or the console says about the memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Declared {
    /// D3D12 upload heap semantics: the CPU writes, the GPU reads.
    UploadHeap,
    /// D3D12 readback heap semantics: the GPU writes, the CPU reads it back.
    ReadbackHeap,
    /// D3D12 default heap semantics: the GPU's own memory.
    DefaultHeap,
    /// Data the CPU writes and the GPU only reads: vertices, indices, constants.
    CpuWritesGpuReads,
    /// Memory only the GPU writes: acceleration structures, UAV-only targets.
    GpuWritesOnly,
    /// A DirectStorage destination: written by our own explicit writer.
    DirectStorageDestination,
    /// The destination of an explicit GPU copy.
    CopyDestination,
}

impl Declared {
    pub fn name(self) -> &'static str {
        match self {
            Declared::UploadHeap => "upload_heap",
            Declared::ReadbackHeap => "readback_heap",
            Declared::DefaultHeap => "default_heap",
            Declared::CpuWritesGpuReads => "cpu_writes_gpu_reads",
            Declared::GpuWritesOnly => "gpu_writes_only",
            Declared::DirectStorageDestination => "dstorage_destination",
            Declared::CopyDestination => "copy_destination",
        }
    }
}

/// Where a region's contract comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Contract {
    Declared(Declared),
    /// Proven by analysis. `evidence` says how, for review.
    Proven { ownership: Ownership, cpu_reads_often: bool, evidence: String },
    /// Nothing known: the conservative default.
    Default,
}

impl Contract {
    pub fn name(&self) -> &'static str {
        match self {
            Contract::Declared(d) => d.name(),
            Contract::Proven { .. } => "proven",
            Contract::Default => "default",
        }
    }

    /// The ownership a page starts with.
    pub fn initial_ownership(&self) -> Ownership {
        match self {
            Contract::Declared(d) => match d {
                Declared::UploadHeap | Declared::CpuWritesGpuReads | Declared::DirectStorageDestination => {
                    Ownership::CpuOwned
                }
                Declared::ReadbackHeap | Declared::DefaultHeap | Declared::GpuWritesOnly | Declared::CopyDestination => {
                    Ownership::GpuOwned
                }
            },
            Contract::Proven { ownership, .. } => *ownership,
            Contract::Default => Ownership::Unknown,
        }
    }

    /// The home a page starts in (D1): VRAM by default for GPU-hot memory;
    /// coherent memory only when the contract says the CPU reads it often or
    /// both sides write it (which includes "nothing known").
    pub fn initial_home(&self) -> Home {
        let cpu_reads_often = match self {
            Contract::Declared(d) => matches!(d, Declared::ReadbackHeap),
            Contract::Proven { cpu_reads_often, .. } => *cpu_reads_often,
            Contract::Default => false,
        };
        if cpu_reads_often || self.initial_ownership().behaves_as_mixed() {
            Home::Coherent
        } else {
            Home::Vram
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every declared contract maps to the ownership and home the ADR gives.
    #[test]
    fn declared_contracts_give_the_adr_ownership_and_home() {
        use Declared::*;
        let table = [
            (UploadHeap, Ownership::CpuOwned, Home::Vram),
            (CpuWritesGpuReads, Ownership::CpuOwned, Home::Vram),
            (DirectStorageDestination, Ownership::CpuOwned, Home::Vram),
            (DefaultHeap, Ownership::GpuOwned, Home::Vram),
            (GpuWritesOnly, Ownership::GpuOwned, Home::Vram),
            (CopyDestination, Ownership::GpuOwned, Home::Vram),
            (ReadbackHeap, Ownership::GpuOwned, Home::Coherent),
        ];
        for (d, own, home) in table {
            let c = Contract::Declared(d);
            assert_eq!((c.initial_ownership(), c.initial_home()), (own, home), "{}", d.name());
        }
    }

    /// Nothing known means Mixed behaviour in coherent memory (D3).
    #[test]
    fn the_default_contract_is_the_conservative_one() {
        assert_eq!(Contract::Default.initial_ownership(), Ownership::Unknown);
        assert!(Contract::Default.initial_ownership().behaves_as_mixed());
        assert_eq!(Contract::Default.initial_home(), Home::Coherent);
    }

    /// A proof carries its own ownership and read pattern.
    #[test]
    fn a_proven_contract_is_taken_as_proven() {
        let c = Contract::Proven { ownership: Ownership::GpuOwned, cpu_reads_often: false, evidence: "test".into() };
        assert_eq!((c.initial_ownership(), c.initial_home()), (Ownership::GpuOwned, Home::Vram));
        let c = Contract::Proven { ownership: Ownership::CpuOwned, cpu_reads_often: true, evidence: "test".into() };
        assert_eq!(c.initial_home(), Home::Coherent);
    }
}
