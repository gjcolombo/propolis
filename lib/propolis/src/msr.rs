// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Handling for model-specific registers (MSRs).

use std::sync::{Arc, Mutex};

use crate::util::aspace::{ASpace, Error as ASpaceError};
use thiserror::Error;

#[usdt::provider(provider = "propolis")]
mod probes {
    fn msr_read(id: u32, val: u64, handled: u8, response: u8) {}
    fn msr_write(id: u32, val: u64, handled: u8, response: u8) {}
}

/// The 32-bit identifier for a specific MSR.
#[derive(Clone, Copy, Debug)]
pub struct MsrId(pub u32);

#[derive(Debug, Error)]
pub enum Error {
    #[error("address space operation failed")]
    ASpace(#[from] ASpaceError),
}

/// An operation on an MSR.
pub enum MsrOp<'a> {
    /// The guest executed RDMSR. The returned value (if any) is written to the
    /// supplied `u64`.
    Read(&'a mut u64),

    /// The guest executed WRMSR and passed the supplied `u64` as an operand.
    Write(u64),
}

/// The result of an operation on an MSR.
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum MsrResponse {
    /// The operation completed successfully. If it was a read, the caller
    /// should copy the value returned in the [`MsrOp`] into guest edx:eax
    /// before returning.
    Handled = 0,

    /// The operation is rejected and the caller should inject #GP into the CPU
    /// that accessed the MSR.
    GpException = 1,
}

/// The default response to operations on MSRs for which no handler was
/// installed.
#[derive(Clone, Copy, Debug)]
pub enum DefaultMsrResponse {
    /// The operation has no effect. Reads return 0 in edx:eax.
    IgnoreAndReturnZero,

    /// The operation is rejected and the caller should inject #GP into the CPU
    /// that accessed the MSR.
    GpException,
}

impl From<DefaultMsrResponse> for MsrResponse {
    fn from(value: DefaultMsrResponse) -> Self {
        match value {
            DefaultMsrResponse::IgnoreAndReturnZero => MsrResponse::Handled,
            DefaultMsrResponse::GpException => MsrResponse::GpException,
        }
    }
}

/// A handler for MSR operations.
pub type MsrFn = dyn Fn(MsrId, MsrOp) -> MsrResponse + Send + Sync + 'static;

/// "Well, I'll tell you what. I'm going to give you a promotion. Welcome
/// aboard, MSR Manager."
///
/// "Wow. I'm MSR Manager!"
pub struct MsrManager {
    map: Mutex<ASpace<Arc<MsrFn>>>,
    default_response: DefaultMsrResponse,
}

impl MsrManager {
    /// Creates a new MSR manager. RDMSR and WRMSR operations will receive the
    /// `default_response` unless a handler is installed for the relevant MSR
    /// ID.
    pub fn new(default_response: DefaultMsrResponse) -> Self {
        Self {
            map: Mutex::new(ASpace::new(0, u32::MAX as usize)),
            default_response,
        }
    }

    /// Registers `func` as the handler for the range of MSRs in
    /// [`start`..`len`).
    pub fn register(
        &self,
        start: MsrId,
        len: u32,
        func: Arc<MsrFn>,
    ) -> Result<(), Error> {
        Ok(self.map.lock().unwrap().register(
            start.0 as usize,
            len as usize,
            func,
        )?)
    }

    /// Unregisters the MSR handler that passed `base` as the starting MSR when
    /// it called [`Self::register`].
    pub fn unregister(&self, base: MsrId) -> Result<(), Error> {
        self.map.lock().unwrap().unregister(base.0 as usize)?;
        Ok(())
    }

    /// Handles the RDMSR instruction.
    pub fn rdmsr(
        &self,
        msr: MsrId,
        out: &mut u64,
    ) -> Result<MsrResponse, Error> {
        let result = self.do_msr_op(msr, MsrOp::Read(out));
        if let Ok((response, handled)) = result {
            probes::msr_read!(|| (msr.0, *out, handled as u8, response as u8));
        }

        result.map(|r| r.0)
    }

    /// Handles the WRMSR instruction.
    pub fn wrmsr(&self, msr: MsrId, value: u64) -> Result<MsrResponse, Error> {
        let result = self.do_msr_op(msr, MsrOp::Write(value));
        if let Ok((response, handled)) = result {
            probes::msr_write!(|| (
                msr.0,
                value,
                handled as u8,
                response as u8
            ));
        }

        result.map(|r| r.0)
    }

    fn do_msr_op(
        &self,
        msr: MsrId,
        op: MsrOp,
    ) -> Result<(MsrResponse, bool), Error> {
        let map = self.map.lock().unwrap();
        let handler = match map.region_at(msr.0 as usize) {
            Ok((_start, _len, handler)) => handler,
            Err(ASpaceError::NotFound) => {
                if let DefaultMsrResponse::IgnoreAndReturnZero =
                    self.default_response
                {
                    if let MsrOp::Read(out) = op {
                        *out = 0;
                    }
                }

                return Ok((self.default_response.into(), false));
            }
            Err(e) => return Err(e.into()),
        };

        let handler = Arc::clone(handler);
        drop(map);
        Ok((handler(msr, op), true))
    }
}
