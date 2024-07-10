// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Functions for computing the CPUID settings to apply to a new instance spec.

use propolis_api_types::instance_spec::components::board::CpuidEntry;

macro_rules! cpuid_leaf {
    ($leaf:literal, $eax:literal, $ebx:literal, $ecx:literal, $edx:literal) => {
        CpuidEntry {
            leaf: $leaf,
            subleaf: None,
            eax: $eax,
            ebx: $ebx,
            ecx: $ecx,
            edx: $edx,
        }
    };
}

macro_rules! cpuid_subleaf {
    ($leaf:literal, $sub:literal, $eax:literal, $ebx:literal, $ecx:literal, $edx:literal) => {
        CpuidEntry {
            leaf: $leaf,
            subleaf: Some($sub),
            eax: $eax,
            ebx: $ebx,
            ecx: $ecx,
            edx: $edx,
        }
    };
}

/// The CPUID definitions for V1 of the Milan-compatible CPU platform. See RFD
/// 314.
pub(super) const MILAN_V1: [CpuidEntry; 32] = [
    cpuid_leaf!(0x0, 0x0000000D, 0x68747541, 0x444D4163, 0x69746E65),
    cpuid_leaf!(0x1, 0x00A00F11, 0x00000800, 0xF6FA3203, 0x078BFBFF),
    cpuid_leaf!(0x5, 0x00000000, 0x00000000, 0x00000000, 0x00000000),
    cpuid_leaf!(0x6, 0x00000002, 0x00000000, 0x00000000, 0x00000000),
    cpuid_subleaf!(0x7, 0x0, 0x00000000, 0x219C03A9, 0x00000000, 0x00000000),
    cpuid_subleaf!(0xB, 0x0, 0x00000001, 0x00000002, 0x00000100, 0x00000000),
    cpuid_subleaf!(0xB, 0x1, 0x00000000, 0x00000000, 0x00000201, 0x00000000),
    cpuid_subleaf!(0xD, 0x0, 0x00000007, 0x00000000, 0x00000340, 0x00000000),
    cpuid_subleaf!(0xD, 0x1, 0x00000007, 0x00000340, 0x00000000, 0x00000000),
    cpuid_subleaf!(0xD, 0x2, 0x00000100, 0x00000240, 0x00000000, 0x00000000),
    cpuid_leaf!(0x80000000, 0x80000021, 0x68747541, 0x444D4163, 0x69746E65),
    cpuid_leaf!(0x80000001, 0x00A00F11, 0x40000000, 0x444001F0, 0x27D3FBFF),
    cpuid_leaf!(0x80000002, 0x73736F72, 0x726F6365, 0x31332050, 0x43203737),
    cpuid_leaf!(0x80000003, 0x20455059, 0x00414D44, 0x00000000, 0x00000000),
    cpuid_leaf!(0x80000004, 0x00000000, 0x00000000, 0x00000000, 0x00000000),
    cpuid_leaf!(0x80000005, 0xFF40FF40, 0xFF40FF40, 0x20080140, 0x20080140),
    cpuid_leaf!(0x80000006, 0x08002200, 0x68004200, 0x02006140, 0x01009140),
    cpuid_leaf!(0x80000007, 0x00000000, 0x00000000, 0x00000000, 0x00000000),
    cpuid_leaf!(0x80000008, 0x00003030, 0x111ED205, 0x00000000, 0x00000000),
    cpuid_leaf!(0x8000000A, 0x00000000, 0x00000000, 0x00000000, 0x00000000),
    cpuid_leaf!(0x80000019, 0xF040F040, 0xF040F040, 0x00000000, 0x00000000),
    cpuid_leaf!(0x8000001A, 0x00000006, 0x00000000, 0x00000000, 0x00000000),
    cpuid_leaf!(0x8000001B, 0x00000000, 0x00000000, 0x00000000, 0x00000000),
    cpuid_leaf!(0x8000001C, 0x00000000, 0x00000000, 0x00000000, 0x00000000),
    cpuid_subleaf!(
        0x8000001D, 0x0, 0x00000121, 0x01C0003F, 0x0000003F, 0x00000000
    ),
    cpuid_subleaf!(
        0x8000001D, 0x1, 0x00000122, 0x01C0003F, 0x0000003F, 0x00000000
    ),
    cpuid_subleaf!(
        0x8000001D, 0x2, 0x00000143, 0x01C0003F, 0x000003FF, 0x00000002
    ),
    cpuid_subleaf!(
        0x8000001D, 0x3, 0x00000163, 0x03C0003F, 0x00007FFF, 0x00000001
    ),
    cpuid_subleaf!(
        0x8000001D, 0x4, 0x00000000, 0x00000000, 0x00000000, 0x00000000
    ),
    cpuid_leaf!(0x8000001E, 0x00000000, 0x00000100, 0x00000000, 0x00000000),
    cpuid_leaf!(0x8000001F, 0x00000000, 0x00000100, 0x00000000, 0x00000000),
    cpuid_leaf!(0x80000021, 0x0000002D, 0x00000100, 0x00000000, 0x00000000),
];
