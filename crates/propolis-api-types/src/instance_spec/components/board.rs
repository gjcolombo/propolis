// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! VM mainboard components. Every VM has a board, even if it has no other
//! peripherals.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::instance_spec::migration::{
    ElementCompatibilityError, MigrationElement,
};

/// An Intel 440FX-compatible chipset.
#[derive(
    Clone, Copy, Deserialize, Serialize, Debug, PartialEq, Eq, JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct I440Fx {
    /// Specifies whether the chipset should allow PCI configuration space
    /// to be accessed through the PCIe extended configuration mechanism.
    pub enable_pcie: bool,
}

impl MigrationElement for I440Fx {
    fn can_migrate_from_element(
        &self,
        other: &Self,
    ) -> Result<(), ElementCompatibilityError> {
        if self.enable_pcie != other.enable_pcie {
            Err(MigrationCompatibilityError::PcieMismatch(
                self.enable_pcie,
                other.enable_pcie,
            )
            .into())
        } else {
            Ok(())
        }
    }
}

/// A kind of virtual chipset.
#[derive(
    Clone, Copy, Deserialize, Serialize, Debug, PartialEq, Eq, JsonSchema,
)]
#[serde(
    deny_unknown_fields,
    rename_all = "snake_case",
    tag = "type",
    content = "value"
)]
pub enum Chipset {
    /// An Intel 440FX-compatible chipset.
    I440Fx(I440Fx),
}

impl MigrationElement for Chipset {
    fn can_migrate_from_element(
        &self,
        other: &Self,
    ) -> Result<(), ElementCompatibilityError> {
        let (Self::I440Fx(this), Self::I440Fx(other)) = (self, other);
        this.can_migrate_from_element(other)
    }
}

/// A single CPUID entry.
#[derive(
    Clone,
    Copy,
    Deserialize,
    Serialize,
    Debug,
    PartialEq,
    Eq,
    JsonSchema,
    PartialOrd,
    Ord,
)]
#[serde(deny_unknown_fields)]
pub struct CpuidEntry {
    /// The leaf/function ID (passed in eax).
    pub leaf: u32,

    /// An optional subleaf/index ID (passed in ecx).
    pub subleaf: Option<u32>,

    /// The value to return in eax.
    pub eax: u32,

    /// The value to return in ebx.
    pub ebx: u32,

    /// The value to return in ecx.
    pub ecx: u32,

    /// The value to return in edx.
    pub edx: u32,
}

impl std::fmt::Display for CpuidEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "leaf {:x}", self.leaf)?;
        if let Some(subleaf) = self.subleaf {
            write!(f, ", subleaf {:x}", subleaf)?;
        }
        write!(
            f,
            ": [{:x}, {:x}, {:x}, {:x}",
            self.eax, self.ebx, self.ecx, self.edx
        )
    }
}

impl MigrationElement for CpuidEntry {
    fn can_migrate_from_element(
        &self,
        other: &Self,
    ) -> Result<(), ElementCompatibilityError> {
        if self != other {
            Err(ElementCompatibilityError::BoardsIncompatible(
                MigrationCompatibilityError::CpuidEntryMismatch(*self, *other),
            ))
        } else {
            Ok(())
        }
    }
}

/// The CPUID values to display to the guest.
#[derive(
    Clone, Deserialize, Serialize, Debug, PartialEq, Eq, JsonSchema, Default,
)]
#[serde(
    deny_unknown_fields,
    rename_all = "snake_case",
    tag = "type",
    content = "value"
)]
pub enum Cpuid {
    /// Use bhyve's default CPUID values.
    #[default]
    BhyveDefault,

    /// Use an explicit set of CPUID values.
    /// TODO(gjc): vendor information
    Entries(Vec<CpuidEntry>),
}

impl Cpuid {
    pub fn mode(&self) -> &'static str {
        match self {
            Self::BhyveDefault => "bhyve",
            Self::Entries(_) => "explicit",
        }
    }
}

impl MigrationElement for Cpuid {
    fn can_migrate_from_element(
        &self,
        other: &Self,
    ) -> Result<(), ElementCompatibilityError> {
        match (self, other) {
            (Self::BhyveDefault, Self::BhyveDefault) => Ok(()),
            (Self::Entries(entries), Self::Entries(other_entries)) => {
                if entries.len() != other_entries.len() {
                    return Err(ElementCompatibilityError::BoardsIncompatible(
                        MigrationCompatibilityError::CpuidEntryLengthMismatch(
                            entries.len(),
                            other_entries.len(),
                        ),
                    ));
                }

                // Sort the entries in each array so that it's possible to
                // compare element-wise.
                let mut entries = entries.clone();
                let mut other_entries = other_entries.clone();
                entries.sort_unstable();
                other_entries.sort_unstable();
                for (this, other) in std::iter::zip(entries, other_entries) {
                    this.can_migrate_from_element(&other)?;
                }

                Ok(())
            }
            _ => Err(ElementCompatibilityError::BoardsIncompatible(
                MigrationCompatibilityError::CpuidModeMismatch(
                    self.mode(),
                    other.mode(),
                ),
            )),
        }
    }
}

/// A VM's mainboard.
#[derive(Clone, Deserialize, Serialize, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Board {
    /// The number of virtual logical processors attached to this VM.
    pub cpus: u8,

    /// The amount of guest RAM attached to this VM.
    pub memory_mb: u64,

    /// The chipset to expose to guest software.
    pub chipset: Chipset,

    /// The VM's CPUID setting.
    pub cpuid: Cpuid,
    // TODO: NUMA topology.
}

impl Default for Board {
    fn default() -> Self {
        Self {
            cpus: 0,
            memory_mb: 0,
            chipset: Chipset::I440Fx(I440Fx { enable_pcie: false }),
            cpuid: Cpuid::BhyveDefault,
        }
    }
}

impl MigrationElement for Board {
    fn can_migrate_from_element(
        &self,
        other: &Self,
    ) -> Result<(), ElementCompatibilityError> {
        if self.cpus != other.cpus {
            Err(MigrationCompatibilityError::CpuCount(self.cpus, other.cpus)
                .into())
        } else if self.memory_mb != other.memory_mb {
            Err(MigrationCompatibilityError::MemorySize(
                self.memory_mb,
                other.memory_mb,
            )
            .into())
        } else if let Err(e) =
            self.chipset.can_migrate_from_element(&other.chipset)
        {
            Err(e)
        } else if let Err(e) = self.cpuid.can_migrate_from_element(&other.cpuid)
        {
            Err(e)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Error)]
pub enum MigrationCompatibilityError {
    #[error("Boards have different CPU counts (self: {0}, other: {1})")]
    CpuCount(u8, u8),

    #[error("Boards have different memory amounts (self: {0}, other: {1})")]
    MemorySize(u64, u64),

    #[error("Chipsets have different PCIe settings (self: {0}, other: {1})")]
    PcieMismatch(bool, bool),

    #[error("CPUID mode mismatch (self: {0}, other: {1})")]
    CpuidModeMismatch(&'static str, &'static str),

    #[error(
        "Explicit CPUID entries have different lengths (self: {0}, other: {1})"
    )]
    CpuidEntryLengthMismatch(usize, usize),

    #[error("CPUID entry mismatch (self: {0}, other: {1})")]
    CpuidEntryMismatch(CpuidEntry, CpuidEntry),
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn compatible_boards() {
        let b1 = Board {
            cpus: 8,
            memory_mb: 8192,
            chipset: Chipset::I440Fx(I440Fx { enable_pcie: false }),
            cpuid: Cpuid::BhyveDefault,
        };

        assert!(b1.can_migrate_from_element(&b1).is_ok());
    }

    #[test]
    fn incompatible_boards() {
        let b1 = Board {
            cpus: 4,
            memory_mb: 4096,
            chipset: Chipset::I440Fx(I440Fx { enable_pcie: true }),
            cpuid: Cpuid::BhyveDefault,
        };

        let mut b2 = b1.clone();
        b2.cpus = 8;
        assert!(b1.can_migrate_from_element(&b2).is_err());

        b2 = b1.clone();
        b2.memory_mb *= 2;
        assert!(b1.can_migrate_from_element(&b2).is_err());

        b2 = b1.clone();
        b2.chipset = Chipset::I440Fx(I440Fx { enable_pcie: false });
        assert!(b1.can_migrate_from_element(&b2).is_err());

        b2 = b1.clone();
        b2.cpuid = Cpuid::Entries(vec![]);
        assert!(b1.can_migrate_from_element(&b2).is_err());
    }

    #[test]
    fn cpuid_both_bhyve() {
        let c1 = Cpuid::BhyveDefault;
        let c2 = Cpuid::BhyveDefault;
        assert!(c1.can_migrate_from_element(&c2).is_ok());
    }

    #[test]
    fn cpuid_matching_entries() {
        let entries = vec![
            CpuidEntry {
                leaf: 0,
                subleaf: None,
                eax: 0,
                ebx: 0,
                ecx: 0,
                edx: 0,
            },
            CpuidEntry {
                leaf: 1,
                subleaf: None,
                eax: 1,
                ebx: 2,
                ecx: 3,
                edx: 4,
            },
            CpuidEntry {
                leaf: 2,
                subleaf: Some(0),
                eax: 0xAAAAAAAA,
                ebx: 0xBBBBBBBB,
                ecx: 0xCCCCCCCC,
                edx: 0xDDDDDDDD,
            },
            CpuidEntry {
                leaf: 3,
                subleaf: Some(1),
                eax: 7,
                ebx: 49,
                ecx: 343,
                edx: 2401,
            },
        ];

        let c1 = Cpuid::Entries(entries.clone());
        let c2 = c1.clone();
        c1.can_migrate_from_element(&c2).unwrap();

        let mut swizzled = entries.clone();
        swizzled.swap(0, 2);
        swizzled.swap(1, 3);
        let c2 = Cpuid::Entries(swizzled);
        c1.can_migrate_from_element(&c2).unwrap();
    }
}
