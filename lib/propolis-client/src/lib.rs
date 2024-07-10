// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! A client for the Propolis hypervisor frontend's server API.

progenitor::generate_api!(
    spec = "../../openapi/propolis-server.json",
    interface = Builder,
    tags = Separate,
    patch = {
        // Some Crucible-related bits are re-exported through simulated
        // sled-agent and thus require JsonSchema
        DiskRequest = { derives = [Clone, Debug, schemars::JsonSchema, Serialize, Deserialize] },
        VolumeConstructionRequest = { derives = [Clone, Debug, schemars::JsonSchema, Serialize, Deserialize] },
        CrucibleOpts = { derives = [Clone, Debug, schemars::JsonSchema, Serialize, Deserialize] },
        Slot = { derives = [Copy, Clone, Debug, schemars::JsonSchema, Serialize, Deserialize] },

        PciPath = { derives = [
            Copy, Clone, Debug, Ord, Eq, PartialEq, PartialOrd, Serialize, Deserialize
        ] },
    },
    // Allow direct use of the virtual platform type to get "free" access to its
    // `FromStr` impl. (This also keeps Progenitor from stripping the
    // major/minor version delimiter from the enum variant names.)
    replace = {
        VirtualPlatform = oxide_virtual_platforms::VirtualPlatform,
    }
);

pub mod instance_spec;
pub mod support;

// Re-export the virtual platform type since it appears in the generated API.
pub use oxide_virtual_platforms::VirtualPlatform;
