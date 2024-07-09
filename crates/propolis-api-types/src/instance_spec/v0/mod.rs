// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Version 0 of a fully-composed instance specification.
//!
//! V0 specs contain a board and an arbitrary set of components.
//!
//! # Versioning and compatibility
//!
//! Changes to structs and enums in this module must be backward-compatible
//! (i.e. new code must be able to deserialize specs created by old version sof
//! the module). Breaking changes to the spec structure must be turned into a
//! new specification version. Note that the common case of adding a new
//! component to an existing enum in this module is not a compat-brekaing
//! change.
//!
//! Data types in this module should have a `V0` suffix in their names to avoid
//! aliasing with type names in other versions. (Collisions can cause Dropshot
//! to create OpenAPI specs that are missing certain types. See dropshot#383.)

use std::collections::HashMap;

use crate::instance_spec::{
    components,
    migration::{
        ElementCompatibilityError, MigrationCollection,
        MigrationCompatibilityError, MigrationElement,
    },
    PciPath, SpecKey,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::components::{
    backends::{
        BlobStorageBackend, CrucibleStorageBackend, DlpiNetworkBackend,
        FileStorageBackend, VirtioNetworkBackend,
    },
    devices::{
        NvmeDisk, P9fs, PciPciBridge, QemuPvpanic, SerialPort, SoftNpuP9,
        SoftNpuPciPort, SoftNpuPort, VirtioDisk, VirtioNic,
    },
    NetworkDevice, StorageBackend, StorageDevice,
};

pub mod builder;

/// The types of components that can be attached to a VM.
#[derive(Clone, Deserialize, Serialize, Debug, JsonSchema)]
#[serde(deny_unknown_fields, tag = "type", content = "component")]
pub enum ComponentV0 {
    VirtioDisk(VirtioDisk),
    NvmeDisk(NvmeDisk),
    VirtioNic(VirtioNic),
    SerialPort(SerialPort),
    PciPciBridge(PciPciBridge),
    QemuPvpanic(QemuPvpanic),

    /// Only usable in Propolis servers built with the `falcon` feature.
    SoftNpuPciPort(SoftNpuPciPort),

    /// Only usable in Propolis servers built with the `falcon` feature.
    SoftNpuPort(SoftNpuPort),

    /// Only usable in Propolis servers built with the `falcon` feature.
    SoftNpuP9(SoftNpuP9),

    /// Only usable in Propolis servers built with the `falcon` feature.
    P9fs(P9fs),

    CrucibleBackend(CrucibleStorageBackend),
    FileStorageBackend(FileStorageBackend),
    BlobStorageBackend(BlobStorageBackend),
    VionaBackend(VirtioNetworkBackend),
    DlpiBackend(DlpiNetworkBackend),
}

impl ComponentV0 {
    pub fn kind(&self) -> &'static str {
        match self {
            ComponentV0::VirtioDisk(_) => "VirtioDisk",
            ComponentV0::NvmeDisk(_) => "NvmeDisk",
            ComponentV0::VirtioNic(_) => "VirtioNic",
            ComponentV0::SerialPort(_) => "SerialPort",
            ComponentV0::PciPciBridge(_) => "PciPciBridge",
            ComponentV0::QemuPvpanic(_) => "QemuPvpanic",
            ComponentV0::SoftNpuPciPort(_) => "SoftNpuPciPort",
            ComponentV0::SoftNpuPort(_) => "SoftNpuPort",
            ComponentV0::SoftNpuP9(_) => "SoftNpuP9",
            ComponentV0::P9fs(_) => "P9fs",
            ComponentV0::CrucibleBackend(_) => "CrucibleBackend",
            ComponentV0::FileStorageBackend(_) => "FileStorageBackend",
            ComponentV0::BlobStorageBackend(_) => "BlobStorageBackend",
            ComponentV0::VionaBackend(_) => "VionaBackend",
            ComponentV0::DlpiBackend(_) => "DlpiBackend",
        }
    }

    /// Returns the PCI BDF where this component should be attached, or `None`
    /// if the component is not a PCI device.
    pub fn pci_path(&self) -> Option<PciPath> {
        match self {
            Self::VirtioDisk(disk) => Some(disk.pci_path),
            Self::NvmeDisk(disk) => Some(disk.pci_path),
            Self::VirtioNic(nic) => Some(nic.pci_path),
            Self::PciPciBridge(bridge) => Some(bridge.pci_path),
            Self::SoftNpuPciPort(port) => Some(port.pci_path),
            Self::SoftNpuP9(p9) => Some(p9.pci_path),
            Self::P9fs(p9fs) => Some(p9fs.pci_path),
            _ => None,
        }
    }
}

impl MigrationElement for ComponentV0 {
    fn can_migrate_from_element(
        &self,
        other: &Self,
    ) -> Result<(), ElementCompatibilityError> {
        // If the two elements have identical kinds, and that type implements
        // a compatibility check, delegate to that type's check. Otherwise,
        // treat the elements as compatible if they're of the same kind.
        match (self, other) {
            (Self::VirtioDisk(this), Self::VirtioDisk(other)) => {
                this.can_migrate_from_element(other)
            }
            (Self::NvmeDisk(this), Self::NvmeDisk(other)) => {
                this.can_migrate_from_element(other)
            }
            (Self::VirtioNic(this), Self::VirtioNic(other)) => {
                this.can_migrate_from_element(other)
            }
            (Self::SerialPort(this), Self::SerialPort(other)) => {
                this.can_migrate_from_element(other)
            }
            (Self::PciPciBridge(this), Self::PciPciBridge(other)) => {
                this.can_migrate_from_element(other)
            }
            (Self::QemuPvpanic(this), Self::QemuPvpanic(other)) => {
                this.can_migrate_from_element(other)
            }
            (Self::CrucibleBackend(this), Self::CrucibleBackend(other)) => {
                this.can_migrate_from_element(other)
            }
            (
                Self::FileStorageBackend(this),
                Self::FileStorageBackend(other),
            ) => this.can_migrate_from_element(other),
            (
                Self::BlobStorageBackend(this),
                Self::BlobStorageBackend(other),
            ) => this.can_migrate_from_element(other),
            (Self::VionaBackend(this), Self::VionaBackend(other)) => {
                this.can_migrate_from_element(other)
            }
            (Self::DlpiBackend(this), Self::DlpiBackend(other)) => {
                this.can_migrate_from_element(other)
            }
            _ => {
                //
                if std::mem::discriminant(self) == std::mem::discriminant(other)
                {
                    Ok(())
                } else {
                    Err(ElementCompatibilityError::ComponentsIncomparable(
                        self.kind(),
                        other.kind(),
                    ))
                }
            }
        }
    }
}

/// A V0 instance specification, consisting of a board and a set of components
/// to attach to the VM.
#[derive(Default, Clone, Deserialize, Serialize, Debug, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InstanceSpecV0 {
    pub board: components::board::Board,
    pub components: HashMap<SpecKey, ComponentV0>,
}

impl InstanceSpecV0 {
    pub fn can_migrate_from(
        &self,
        other: &Self,
    ) -> Result<(), MigrationCompatibilityError> {
        self.board.can_migrate_from_element(&other.board).map_err(|e| {
            MigrationCompatibilityError::ElementMismatch("board".to_string(), e)
        })?;

        self.components
            .can_migrate_from_collection(&other.components)
            .map_err(|e| {
                MigrationCompatibilityError::CollectionMismatch(
                    "components".to_string(),
                    e,
                )
            })?;

        Ok(())
    }

    pub fn storage_devices(
        &self,
    ) -> impl Iterator<Item = (&String, StorageDevice<'_>)> {
        self.components.iter().filter_map(|(k, v)| {
            match v {
                ComponentV0::VirtioDisk(d) => {
                    Some(StorageDevice::VirtioDisk(d))
                }
                ComponentV0::NvmeDisk(d) => Some(StorageDevice::NvmeDisk(d)),
                _ => None,
            }
            .map(|v| (k, v))
        })
    }

    pub fn storage_backends(
        &self,
    ) -> impl Iterator<Item = (&String, StorageBackend<'_>)> {
        self.components.iter().filter_map(|(k, v)| {
            match v {
                ComponentV0::CrucibleBackend(b) => {
                    Some(StorageBackend::CrucibleBackend(b))
                }
                ComponentV0::FileStorageBackend(b) => {
                    Some(StorageBackend::FileStorageBackend(b))
                }
                ComponentV0::BlobStorageBackend(b) => {
                    Some(StorageBackend::BlobStorageBackend(b))
                }
                _ => None,
            }
            .map(|v| (k, v))
        })
    }

    pub fn network_devices(
        &self,
    ) -> impl Iterator<Item = (&String, NetworkDevice<'_>)> {
        self.components.iter().filter_map(|(k, v)| {
            {
                match v {
                    ComponentV0::VirtioNic(n) => {
                        Some(NetworkDevice::VirtioNic(n))
                    }
                    _ => None,
                }
            }
            .map(|v| (k, v))
        })
    }

    pub fn pci_pci_bridges(
        &self,
    ) -> impl Iterator<Item = (&String, &PciPciBridge)> {
        self.components.iter().filter_map(|(k, v)| match v {
            ComponentV0::PciPciBridge(b) => Some((k, b)),
            _ => None,
        })
    }

    pub fn softnpu_pci_ports(
        &self,
    ) -> impl Iterator<Item = (&String, &SoftNpuPciPort)> {
        self.components.iter().filter_map(|(k, v)| match v {
            ComponentV0::SoftNpuPciPort(p) => Some((k, p)),
            _ => None,
        })
    }

    pub fn softnpu_ports(
        &self,
    ) -> impl Iterator<Item = (&String, &SoftNpuPort)> {
        self.components.iter().filter_map(|(k, v)| match v {
            ComponentV0::SoftNpuPort(p) => Some((k, p)),
            _ => None,
        })
    }

    pub fn softnpu_p9(&self) -> impl Iterator<Item = (&String, &SoftNpuP9)> {
        self.components.iter().filter_map(|(k, v)| match v {
            ComponentV0::SoftNpuP9(p) => Some((k, p)),
            _ => None,
        })
    }

    pub fn p9fs(&self) -> impl Iterator<Item = (&String, &P9fs)> {
        self.components.iter().filter_map(|(k, v)| match v {
            ComponentV0::P9fs(p) => Some((k, p)),
            _ => None,
        })
    }
}
