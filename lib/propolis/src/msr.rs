// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Support for handling accesses to model-specific registers (MSRs).
//!
//! This module's [`MsrManager`] type allows Propolis users both to configure
//! default exit-handling behavior for specific MSRs and to allow other Propolis
//! components to register themselves as handlers for MSRs that they control.

use std::{
    collections::{btree_map, BTreeMap},
    sync::Mutex,
};

use thiserror::Error;

#[usdt::provider(provider = "propolis")]
mod probes {
    fn msr_read(vcpuid: u32, msr: u32, val: u64, handled: u8, gp: u8) {}
    fn msr_write(vcpuid: u32, msr: u32, val: u64, handled: u8, gp: u8) {}
}

/// The 32-bit ID of a specific MSR.
#[derive(Clone, Copy, Debug, PartialOrd, Ord, PartialEq, Eq)]
pub struct MsrId(pub u32);

/// A generic means of disposing of an MSR access.
#[derive(Clone, Copy, Debug)]
pub enum Disposition {
    /// Ignore the access, returning 0 for reads and dropping writes.
    Ignore,

    /// Raise #GP on the accessing CPU.
    GpException,
}

/// A set of read/write disposition rules for a specific MSR.
#[derive(Clone, Copy, Debug)]
pub struct MsrDispositions {
    /// The [`Disposition`] to use for reads.
    pub read: Disposition,

    /// The [`Disposition`] to use for writes.
    pub write: Disposition,
}

/// The result of an RDMSR instruction.
#[derive(Debug)]
pub(crate) enum RdmsrResult {
    /// Return the supplied value to the guest.
    Value(u64),

    /// Raise #GP on the CPU that executed RDMSR.
    GpException,
}

/// The result of a WRMSR instruction.
#[derive(Debug)]
pub(crate) enum WrmsrResult {
    /// The write was successfully handled.
    Handled,

    /// Raise #GP on the CPU that executed WRMSR.
    GpException,
}

enum Discipline {
    Handle(MsrDispositions),
    // TODO: Support delegation to other registered components.
    //
    // The idea here is to have a trait along the lines of
    //
    // trait MsrHandler {
    //     fn rdmsr(VcpuId, MsrId) -> RdmsrResult;
    //     fn wrmsr(Vcpuid, MsrId, Value) -> WrmsrResult;
    // }
    //
    // Then the "delegate" discipline would obtain an Arc<dyn MsrHandler> and
    // dispatch reads/writes to it when appropriate.
}

impl Discipline {
    fn kind(&self) -> &'static str {
        match self {
            Self::Handle(_) => "handle",
        }
    }
}

/// "Well, I'll tell you what. I'm going to give you a promotion. Welcome
/// aboard, MSR Manager."
///
/// "Wow, I'm MSR Manager!"
///
/// Manages the handler disciplines for MSR accesses. An MSR access can either
/// be handled directly by the manager (which can be programmed either to ignore
/// it or to inject #GP) or delegated to some other component that has
/// registered itself as an MSR handler.
///
/// Because the only supported "local" dispositions are to ignore reads, drop
/// writes, or inject exceptions, the manager never needs to store the values
/// the guest writes to MSRs, and therefore does not impl [`Lifecycle`] or
/// participate in live migration. Delegate components that need to save/restore
/// MSR state should impl `Lifecycle` themselves and migrate the MSR values they
/// control.
///
/// [`Lifecycle`]: crate::lifecycle::Lifecycle
pub struct MsrManager {
    /// A mapping from MSR IDs to handler disciplines.
    map: Mutex<BTreeMap<MsrId, Discipline>>,
}

#[derive(Debug, Error)]
#[error("conflicting discipline '{0}' already installed")]
pub struct DisciplineConflict(&'static str);

impl MsrManager {
    /// Creates a new MSR manager.
    pub fn new() -> Self {
        Self { map: Mutex::new(BTreeMap::new()) }
    }

    /// Sets the dispositions for the MSR with the supplied `id`. Returns `Ok`
    /// if no entry for this MSR exists yet and `Err(DisciplineConflict)` if a
    /// handling discipline was already set for this MSR.
    pub fn set_dispositions(
        &self,
        id: MsrId,
        disp: MsrDispositions,
    ) -> Result<(), DisciplineConflict> {
        let mut map = self.map.lock().unwrap();
        match map.entry(id) {
            btree_map::Entry::Vacant(e) => {
                e.insert(Discipline::Handle(disp));
                Ok(())
            }
            btree_map::Entry::Occupied(e) => {
                Err(DisciplineConflict(e.get().kind()))
            }
        }
    }

    /// Handles an RDMSR to the supplied `msr`. Returns `None` if no handling
    /// discipline has been set for the target MSR.
    pub(crate) fn rdmsr(&self, vcpuid: u32, msr: MsrId) -> Option<RdmsrResult> {
        let res = {
            let map = self.map.lock().unwrap();
            map.get(&msr).map(|discipline| {
                let Discipline::Handle(disp) = discipline;
                match disp.read {
                    Disposition::Ignore => RdmsrResult::Value(0),
                    Disposition::GpException => RdmsrResult::GpException,
                }
            })
        };

        probes::msr_read!(|| {
            let (val, handled, gp) = match res {
                None => (0, false, false),
                Some(RdmsrResult::Value(v)) => (v, true, false),
                Some(RdmsrResult::GpException) => (0, true, true),
            };

            (vcpuid, msr.0, val, handled as u8, gp as u8)
        });

        res
    }

    /// Handles an WRMSR to the supplied `msr`. Returns `None` if no handling
    /// discipline has been set for the target MSR.
    pub(crate) fn wrmsr(
        &self,
        vcpuid: u32,
        msr: MsrId,
        value: u64,
    ) -> Option<WrmsrResult> {
        let res = {
            let map = self.map.lock().unwrap();
            map.get(&msr).map(|discipline| {
                let Discipline::Handle(disp) = discipline;
                match disp.read {
                    Disposition::Ignore => WrmsrResult::Handled,
                    Disposition::GpException => WrmsrResult::GpException,
                }
            })
        };

        probes::msr_write!(|| {
            let (handled, gp) = match res {
                None => (false, false),
                Some(WrmsrResult::Handled) => (true, false),
                Some(WrmsrResult::GpException) => (true, true),
            };

            (vcpuid, msr.0, value, handled as u8, gp as u8)
        });

        res
    }
}
