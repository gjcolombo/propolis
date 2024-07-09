// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::BTreeSet;

use crate::types::{
    Board, Chipset, ComponentV0, I440Fx, InstanceSpecV0, PciPath,
    SerialPortNumber,
};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpecBuilderError {
    #[error("A component with name {0} already exists")]
    NameInUse(String),

    #[error("Serial port {0:?} is already specified")]
    SerialPortInUse(SerialPortNumber),

    #[error("A PCI device is already attached at {0:?}")]
    PciPathInUse(PciPath),
}

/// A builder that constructs instance specs incrementally and catches basic
/// errors, such as specifying duplicate component names or specifying multiple
/// devices with the same PCI path.
pub struct SpecBuilder {
    spec: InstanceSpecV0,
    serial_ports: BTreeSet<SerialPortNumber>,
    pci_paths: BTreeSet<PciPath>,
}

impl SpecBuilder {
    pub fn new(cpus: u8, memory_mb: u64) -> Self {
        let board = Board {
            cpus,
            memory_mb,
            chipset: Chipset::I440Fx(I440Fx { enable_pcie: false }),
        };

        Self {
            spec: InstanceSpecV0 { board, components: Default::default() },
            serial_ports: Default::default(),
            pci_paths: Default::default(),
        }
    }

    pub fn finish(self) -> InstanceSpecV0 {
        self.spec
    }

    /// Adds a PCI path to this builder's record of PCI locations with an
    /// attached device. If the path is already in use, returns an error.
    fn register_pci_device(
        &mut self,
        pci_path: PciPath,
    ) -> Result<(), SpecBuilderError> {
        if self.pci_paths.contains(&pci_path) {
            Err(SpecBuilderError::PciPathInUse(pci_path))
        } else {
            self.pci_paths.insert(pci_path);
            Ok(())
        }
    }

    fn register_serial_port(
        &mut self,
        port: SerialPortNumber,
    ) -> Result<(), SpecBuilderError> {
        if self.serial_ports.contains(&port) {
            Err(SpecBuilderError::SerialPortInUse(port))
        } else {
            self.serial_ports.insert(port);
            Ok(())
        }
    }

    pub fn enable_pcie(&mut self) -> &Self {
        self.spec.board.chipset = Chipset::I440Fx(I440Fx { enable_pcie: true });

        self
    }

    pub fn add_component(
        &mut self,
        name: String,
        component: ComponentV0,
    ) -> Result<&Self, SpecBuilderError> {
        if self.spec.components.contains_key(&name) {
            return Err(SpecBuilderError::NameInUse(name));
        }

        fn pci_path(component: &ComponentV0) -> Option<PciPath> {
            match component {
                ComponentV0::VirtioDisk(disk) => Some(disk.pci_path),
                ComponentV0::NvmeDisk(disk) => Some(disk.pci_path),
                ComponentV0::VirtioNic(nic) => Some(nic.pci_path),
                ComponentV0::PciPciBridge(bridge) => Some(bridge.pci_path),
                ComponentV0::SoftNpuPciPort(port) => Some(port.pci_path),
                ComponentV0::SoftNpuP9(p9) => Some(p9.pci_path),
                ComponentV0::P9fs(p9fs) => Some(p9fs.pci_path),
                _ => None,
            }
        }

        if let Some(pci_path) = pci_path(&component) {
            self.register_pci_device(pci_path)?;
        }

        if let ComponentV0::SerialPort(port) = &component {
            self.register_serial_port(port.num)?;
        }

        let _old = self.spec.components.insert(name, component);
        assert!(_old.is_none());

        Ok(self)
    }
}
