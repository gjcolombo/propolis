// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Utilities for reading host-supplied default CPUID values.

use anyhow::Context;
use bhyve_api::{VmmCtlFd, VmmFd};
use propolis_types::{CpuidIdent, CpuidValues, CpuidVendor};

use crate::{
    bits::{AmdExtLeaf1DEax, Leaf1Ecx},
    CpuidSet,
};

struct Vmm {
    fd: VmmFd,
}

impl Drop for Vmm {
    fn drop(&mut self) {
        let _ = self.fd.ioctl_usize(bhyve_api::VM_DESTROY_SELF, 0);
    }
}

fn create_vm() -> anyhow::Result<Vmm> {
    let name = format!("cpuid-gen-{}", std::process::id());
    let mut req =
        bhyve_api::vm_create_req::new(name.as_bytes()).expect("valid VM name");

    let ctl = VmmCtlFd::open()?;
    let _ = unsafe { ctl.ioctl(bhyve_api::VMM_CREATE_VM, &mut req) }?;

    let vm = match VmmFd::open(&name) {
        Ok(vm) => vm,
        Err(e) => {
            // Attempt to manually destroy the VM if we cannot open it
            let _ = ctl.vm_destroy(name.as_bytes());
            return Err(e.into());
        }
    };

    match vm.ioctl_usize(bhyve_api::ioctls::VM_SET_AUTODESTRUCT, 1) {
        Ok(_res) => {}
        Err(e) => {
            // Destroy instance if auto-destruct cannot be set
            let _ = vm.ioctl_usize(bhyve_api::VM_DESTROY_SELF, 0);
            return Err(e.into());
        }
    };

    Ok(Vmm { fd: vm })
}

fn query_cpuid(vm: &Vmm, eax: u32, ecx: u32) -> anyhow::Result<CpuidValues> {
    let mut data = bhyve_api::vm_legacy_cpuid {
        vlc_eax: eax,
        vlc_ecx: ecx,
        ..Default::default()
    };
    unsafe { vm.fd.ioctl(bhyve_api::VM_LEGACY_CPUID, &mut data) }
        .with_context(|| "issuing VM_LEGACY_CPUID({eax}, {ecx})")?;
    Ok(CpuidValues::from([
        data.vlc_eax,
        data.vlc_ebx,
        data.vlc_ecx,
        data.vlc_edx,
    ]))
}

/// Gets the default CPUID values that bhyve supplies to guests in VMs where no
/// explicit CPUID values have been provided.
pub fn get_bhyve_default_cpuid() -> anyhow::Result<CpuidSet> {
    const HYPERVISOR_BASE: u32 = 0x4000_0000;
    const EXTENDED_BASE: u32 = 0x8000_0000;
    let vm = create_vm().context("creating temporary VM to query CPUID")?;
    let mut set = CpuidSet::default();
    let vendor = set.vendor;

    let mut add_to_set = |ident: CpuidIdent, values: CpuidValues| {
        set.insert(ident, values)
            .with_context(|| format!("inserting {ident:?}"))
    };

    let std = query_cpuid(&vm, 0, 0)?;
    let extd = query_cpuid(&vm, EXTENDED_BASE, 0)?;

    let mut xsave_supported = false;
    for eax in 0..=std.eax {
        let data = query_cpuid(&vm, eax, 0)?;
        match eax {
            0x1 => {
                // Remember whether bhyve will advertise XSAVE support, since
                // this determines whether to enumerate and add leaf D's
                // subleaves.
                if data.ecx & Leaf1Ecx::XSAVE.bits() != 0 {
                    xsave_supported = true;
                }

                add_to_set(CpuidIdent::leaf(eax), data)?;
            }
            0x7 => {
                let max_subleaf = data.eax;
                add_to_set(CpuidIdent::subleaf(eax, 0), data)?;
                for subleaf in 1..=max_subleaf {
                    let subleaf_data = query_cpuid(&vm, eax, subleaf)?;
                    add_to_set(
                        CpuidIdent::subleaf(eax, subleaf),
                        subleaf_data,
                    )?;
                }
            }
            0xB => {
                // AMD's documentation only advertises support for two subleaves
                // of leaf B. Intel allows more subleaves in theory, but bhyve's
                // legacy CPUID implementation (at least at this writing) only
                // returns usable data for the first two subleaves.
                add_to_set(CpuidIdent::subleaf(eax, 0), data)?;
                add_to_set(
                    CpuidIdent::subleaf(eax, 1),
                    query_cpuid(&vm, eax, 1)?,
                )?;
            }
            0xD if xsave_supported => {
                add_to_set(CpuidIdent::subleaf(eax, 0), data)?;
                let xcr0_supported =
                    u64::from(data.eax) | (u64::from(data.edx) << 32);

                let data = query_cpuid(&vm, eax, 1)?;
                add_to_set(CpuidIdent::subleaf(eax, 1), data)?;
                let xss_supported =
                    u64::from(data.eax) | (u64::from(data.edx) << 32);

                for ecx in 2..64 {
                    let mask = 1u64 << ecx;
                    let in_xcr0 = (xcr0_supported & mask) != 0;
                    let in_xss = (xss_supported & mask) != 0;
                    if !in_xcr0 && !in_xss {
                        continue;
                    }

                    let data = query_cpuid(&vm, eax, ecx)?;
                    add_to_set(CpuidIdent::subleaf(eax, ecx), data)?;
                }
            }
            _ => {
                add_to_set(CpuidIdent::leaf(eax), data)?;
            }
        }
    }

    for eax in EXTENDED_BASE..=extd.eax {
        let data = query_cpuid(&vm, eax, 0)?;
        match (eax, vendor) {
            (0x8000_001D, CpuidVendor::Amd) => {
                for ecx in 0..=u32::MAX {
                    let data = query_cpuid(&vm, eax, ecx)?;
                    let eax_out = AmdExtLeaf1DEax::from_bits_retain(data.eax);
                    if eax_out.cache_type().is_null() {
                        break;
                    }

                    add_to_set(CpuidIdent::subleaf(eax, ecx), data)?;
                }
            }
            _ => {
                add_to_set(CpuidIdent::leaf(eax), data)?;
            }
        }
    }

    // Report bhyve's default hypervisor ID (just to avoid exposing a blank
    // hypervisor identifier string to the guest).
    let data = query_cpuid(&vm, HYPERVISOR_BASE, 0)?;
    add_to_set(CpuidIdent::leaf(HYPERVISOR_BASE), data)?;

    Ok(set)
}
