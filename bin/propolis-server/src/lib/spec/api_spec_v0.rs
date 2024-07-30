// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Conversions from version-0 instance specs in the [`propolis_api_types`]
//! crate to the internal [`super::Spec`] representation.

use std::collections::HashMap;

use propolis_api_types::instance_spec::{
    components::{
        backends::{DlpiNetworkBackend, VirtioNetworkBackend},
        devices::SerialPort as SerialPortDesc,
    },
    v0::{ComponentV0, InstanceSpecV0},
};
use thiserror::Error;

#[cfg(feature = "falcon")]
use propolis_api_types::instance_spec::components::devices::SoftNpuPort as SoftNpuPortSpec;

use super::{
    builder::{SpecBuilder, SpecBuilderError},
    Disk, Nic, QemuPvpanic, SerialPortUser, Spec, StorageBackend,
    StorageDevice,
};

#[cfg(feature = "falcon")]
use super::SoftNpuPort;

#[derive(Debug, Error)]
pub(crate) enum ApiSpecParseError {
    #[error(transparent)]
    BuilderError(#[from] SpecBuilderError),

    #[error("storage backend {0} not found for device {1}")]
    StorageBackendNotFound(String, String),

    #[error("network backend {0} not found for device {1}")]
    NetworkBackendNotFound(String, String),

    #[error("softnpu component {0} compiled out")]
    SoftNpuCompiledOut(String),

    #[error("backend {0} not used by any device")]
    BackendNotUsed(String),
}

impl From<Spec> for InstanceSpecV0 {
    fn from(val: Spec) -> Self {
        let mut spec =
            InstanceSpecV0 { board: val.board, ..Default::default() };

        for (disk_name, disk) in val.disks {
            let backend_name = disk.device_spec.backend_name().to_owned();
            let _old =
                spec.components.insert(disk_name, disk.device_spec.into());

            assert!(_old.is_none());

            let _old =
                spec.components.insert(backend_name, disk.backend_spec.into());

            assert!(_old.is_none());
        }

        for (nic_name, nic) in val.nics {
            let backend_name = nic.device_spec.backend_name.clone();
            let _old = spec
                .components
                .insert(nic_name, ComponentV0::VirtioNic(nic.device_spec));

            assert!(_old.is_none());

            let _old = spec.components.insert(
                backend_name,
                ComponentV0::VirtioNetworkBackend(nic.backend_spec),
            );

            assert!(_old.is_none());
        }

        for (name, desc) in val.serial {
            if desc.user == SerialPortUser::Standard {
                let _old = spec.components.insert(
                    name,
                    ComponentV0::SerialPort(SerialPortDesc { num: desc.num }),
                );

                assert!(_old.is_none());
            }
        }

        for (bridge_name, bridge) in val.pci_pci_bridges {
            let _old = spec
                .components
                .insert(bridge_name, ComponentV0::PciPciBridge(bridge));

            assert!(_old.is_none());
        }

        if let Some(pvpanic) = val.pvpanic {
            let _old = spec.components.insert(
                pvpanic.name.clone(),
                ComponentV0::QemuPvpanic(pvpanic.spec),
            );

            assert!(_old.is_none());
        }

        #[cfg(feature = "falcon")]
        {
            if let Some(softnpu_pci) = val.softnpu.pci_port {
                let _old = spec.components.insert(
                    format!("softnpu-pci-{}", softnpu_pci.pci_path),
                    ComponentV0::SoftNpuPciPort(softnpu_pci),
                );

                assert!(_old.is_none());
            }

            if let Some(p9) = val.softnpu.p9_device {
                let _old = spec.components.insert(
                    format!("softnpu-p9-{}", p9.pci_path),
                    ComponentV0::SoftNpuP9(p9),
                );

                assert!(_old.is_none());
            }

            if let Some(p9fs) = val.softnpu.p9fs {
                let _old = spec.components.insert(
                    format!("p9fs-{}", p9fs.pci_path),
                    ComponentV0::P9fs(p9fs),
                );

                assert!(_old.is_none());
            }

            for (port_name, port) in val.softnpu.ports {
                let _old = spec.components.insert(
                    port_name.clone(),
                    ComponentV0::SoftNpuPort(SoftNpuPortSpec {
                        name: port_name,
                        backend_name: port.backend_name.clone(),
                    }),
                );

                assert!(_old.is_none());

                let _old = spec.components.insert(
                    port.backend_name,
                    ComponentV0::DlpiNetworkBackend(port.backend_spec),
                );

                assert!(_old.is_none());
            }
        }

        spec
    }
}

impl TryFrom<InstanceSpecV0> for Spec {
    type Error = ApiSpecParseError;

    fn try_from(value: InstanceSpecV0) -> Result<Self, Self::Error> {
        let mut builder = SpecBuilder::with_board(value.board);
        let mut devices: Vec<(String, ComponentV0)> = vec![];
        let mut storage_backends: HashMap<String, StorageBackend> =
            HashMap::new();
        let mut viona_backends: HashMap<String, VirtioNetworkBackend> =
            HashMap::new();
        let mut dlpi_backends: HashMap<String, DlpiNetworkBackend> =
            HashMap::new();

        for (name, component) in value.components.into_iter() {
            match component {
                ComponentV0::CrucibleStorageBackend(_)
                | ComponentV0::FileStorageBackend(_)
                | ComponentV0::BlobStorageBackend(_) => {
                    storage_backends.insert(
                        name,
                        component.try_into().expect(
                            "component is known to be a storage backend",
                        ),
                    );
                }
                ComponentV0::VirtioNetworkBackend(viona) => {
                    viona_backends.insert(name, viona);
                }
                ComponentV0::DlpiNetworkBackend(dlpi) => {
                    dlpi_backends.insert(name, dlpi);
                }
                device => {
                    devices.push((name, device));
                }
            }
        }

        for (device_name, device_spec) in devices {
            match device_spec {
                ComponentV0::VirtioDisk(_) | ComponentV0::NvmeDisk(_) => {
                    let device_spec = StorageDevice::try_from(device_spec)
                        .expect("component is known to be a disk");

                    let (_, backend_spec) = storage_backends
                        .remove_entry(device_spec.backend_name())
                        .ok_or_else(|| {
                            ApiSpecParseError::StorageBackendNotFound(
                                device_spec.backend_name().to_owned(),
                                device_name.clone(),
                            )
                        })?;

                    builder.add_storage_device(
                        device_name,
                        Disk { device_spec, backend_spec },
                    )?;
                }
                ComponentV0::VirtioNic(nic) => {
                    let (_, backend_spec) = viona_backends
                        .remove_entry(&nic.backend_name)
                        .ok_or_else(|| {
                            ApiSpecParseError::NetworkBackendNotFound(
                                nic.backend_name.clone(),
                                device_name.clone(),
                            )
                        })?;

                    builder.add_network_device(
                        device_name,
                        Nic { device_spec: nic, backend_spec },
                    )?;
                }
                ComponentV0::SerialPort(port) => {
                    builder.add_serial_port(device_name, port.num)?;
                }
                ComponentV0::PciPciBridge(bridge) => {
                    builder.add_pci_bridge(device_name, bridge)?;
                }
                ComponentV0::QemuPvpanic(pvpanic) => {
                    builder.add_pvpanic_device(QemuPvpanic {
                        name: device_name,
                        spec: pvpanic,
                    })?;
                }
                #[cfg(not(feature = "falcon"))]
                ComponentV0::SoftNpuPciPort(_)
                | ComponentV0::SoftNpuPort(_)
                | ComponentV0::SoftNpuP9(_)
                | ComponentV0::P9fs(_) => {
                    return Err(ApiSpecParseError::SoftNpuCompiledOut(
                        device_name,
                    ));
                }
                #[cfg(feature = "falcon")]
                ComponentV0::SoftNpuPciPort(port) => {
                    builder.set_softnpu_pci_port(port)?;
                }
                #[cfg(feature = "falcon")]
                ComponentV0::SoftNpuPort(port) => {
                    let (_, backend_spec) = dlpi_backends
                        .remove_entry(&port.backend_name)
                        .ok_or_else(|| {
                            ApiSpecParseError::NetworkBackendNotFound(
                                port.backend_name.clone(),
                                device_name.clone(),
                            )
                        })?;

                    let port = SoftNpuPort {
                        backend_name: port.backend_name,
                        backend_spec,
                    };

                    builder.add_softnpu_port(device_name, port)?;
                }
                #[cfg(feature = "falcon")]
                ComponentV0::SoftNpuP9(p9) => {
                    builder.set_softnpu_p9(p9)?;
                }
                #[cfg(feature = "falcon")]
                ComponentV0::P9fs(p9fs) => {
                    builder.set_p9fs(p9fs)?;
                }
                ComponentV0::CrucibleStorageBackend(_)
                | ComponentV0::FileStorageBackend(_)
                | ComponentV0::BlobStorageBackend(_)
                | ComponentV0::VirtioNetworkBackend(_)
                | ComponentV0::DlpiNetworkBackend(_) => {
                    unreachable!("already filtered out backends")
                }
            }
        }

        if let Some(backend) = storage_backends.into_keys().next() {
            return Err(ApiSpecParseError::BackendNotUsed(backend));
        }

        if let Some(backend) = viona_backends.into_keys().next() {
            return Err(ApiSpecParseError::BackendNotUsed(backend));
        }

        if let Some(backend) = dlpi_backends.into_keys().next() {
            return Err(ApiSpecParseError::BackendNotUsed(backend));
        }

        Ok(builder.finish())
    }
}
