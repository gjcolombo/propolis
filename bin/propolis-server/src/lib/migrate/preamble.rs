// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use propolis_api_types::instance_spec::{
    migration::MigrationCompatibilityError, v0::InstanceSpecV0,
    VersionedInstanceSpec,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug)]
pub(crate) struct Preamble {
    instance_spec: VersionedInstanceSpec,
    pub blobs: Vec<Vec<u8>>,
}

impl Preamble {
    pub fn new(instance_spec: VersionedInstanceSpec) -> Preamble {
        Preamble { instance_spec: instance_spec.clone(), blobs: Vec::new() }
    }

    pub fn is_migration_compatible(
        &self,
        other_spec: &InstanceSpecV0,
    ) -> Result<(), MigrationCompatibilityError> {
        let VersionedInstanceSpec::V0(this_spec) = &self.instance_spec;
        this_spec.can_migrate_from(other_spec)?;

        // TODO: Compare opaque blobs.

        Ok(())
    }
}
