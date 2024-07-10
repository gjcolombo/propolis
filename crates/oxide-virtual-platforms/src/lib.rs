// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Virtual platforms describe the rules for taking an "abstract" instance
//! description in the Oxide control plane and converting it into a "concrete"
//! virtual machine abstraction that Propolis can present to guest software.
//!
//! # Taxonomy & versioning
//!
//! Each virtual platform has a "family" and major and minor version numbers
//! that the control plane uses to schedule instances. These work as follows:
//!
//! - Each instance has a "minimum required" family and may have a minimum
//!   required major version within that family.
//! - Each compute sled advertises the families it supports and, for each
//!   family, the maximum major version of that family that it supports.
//! - When choosing a sled for an instance, the control plane must select a sled
//!   that supports the supplied family at the specified version.
//!
//! For example:
//!
//! - An instance that requires the "AMD Milan" platform can run on Milan-based
//!   sleds but not Rome-based sleds. An instance that uses the Rome
//!   platform can run on either.
//! - A new enlightenment is enabled starting with version 2 of the Milan
//!   platform. An instance that uses the enlightenment can only be scheduled to
//!   a sled that supports Milan v2. Milan v1 instances can be scheduled to any
//!   Milan-compatible sled (but they won't get the enlightenment, even if the
//!   underlying host supports it).
//!
//! # Formatting
//!
//! Virtual platform variants convert to and from strings of the form
//! "family_major_minor".

use std::fmt::Display;
use std::io::ErrorKind;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A virtual platform family describes the general kind of guest CPU platform
/// and guest functionality that will be made available to instances using the
/// family.
///
/// Specific platforms within a family may use different, more specific CPU
/// platform versions or system features. That is, all Milan-family virtual
/// platforms will have an "AMD Milan-compatible" guest CPU platform, but
/// different platforms in the family may differ in the specific features
/// they expose to guests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(test, derive(strum::EnumIter))]
pub enum Family {
    /// The initial Oxide virtual platform, provided for compatibility with
    /// instances that came into being before virtual platforms were added.
    ///
    /// Alias "mvp" in stringified platform IDs.
    OxideMvp,

    /// An AMD Milan-compatible virtual platform.
    ///
    /// Alias "milan" in stringified platform IDs.
    Milan,
}

impl Family {
    pub fn as_str(&self) -> &'static str {
        // N.B. Family identifiers aren't allowed to contain underscores,
        // since those are the delimiters used in full platform ID strings.
        match self {
            Self::OxideMvp => "mvp",
            Self::Milan => "milan",
        }
    }
}

impl Display for Family {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for Family {
    type Err = std::io::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mvp" => Ok(Self::OxideMvp),
            "milan" => Ok(Self::Milan),
            _ => Err(std::io::Error::new(
                ErrorKind::InvalidInput,
                format!("unrecognized virtual platform family: {s}"),
            )),
        }
    }
}

#[derive(
    Clone, Copy, PartialEq, Eq, Debug, JsonSchema, Serialize, Deserialize,
)]
#[cfg_attr(test, derive(strum::EnumIter))]
#[serde(rename_all = "snake_case")]
pub enum VirtualPlatform {
    OxideMvp,
    MilanV1_0,
}

impl VirtualPlatform {
    pub fn family(&self) -> Family {
        match self {
            Self::OxideMvp => Family::OxideMvp,
            Self::MilanV1_0 => Family::Milan,
        }
    }

    pub fn version(&self) -> (u32, u32) {
        match self {
            Self::OxideMvp => (0, 0),
            Self::MilanV1_0 => (1, 0),
        }
    }
}

impl Display for VirtualPlatform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}_{}_{}", self.family(), self.version().0, self.version().1)
    }
}

impl FromStr for VirtualPlatform {
    type Err = std::io::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        fn make_error(s: String) -> std::io::Error {
            std::io::Error::new(ErrorKind::InvalidInput, s)
        }

        let mut fields = Vec::with_capacity(3);
        for f in s.split('_') {
            fields.push(f);
        }

        if fields.len() != 3 {
            return Err(make_error(format!(
                "expected 3 fields in virtual platform string {}, got {}",
                s,
                fields.len()
            )));
        }

        let family = Family::from_str(fields[0])?;
        let major = u32::from_str(fields[1]).map_err(|e| {
            make_error(format!(
                "failed to parse major version {}: {}",
                fields[1], e
            ))
        })?;

        let minor = u32::from_str(fields[2]).map_err(|e| {
            make_error(format!(
                "failed to parse minor version {}: {}",
                fields[2], e
            ))
        })?;

        match (family, major, minor) {
            (Family::OxideMvp, 0, 0) => Ok(Self::OxideMvp),
            (Family::Milan, 1, 0) => Ok(Self::MilanV1_0),
            _ => Err(make_error(format!(
                "unrecognized platform: \
                 family {family}, \
                 major version {major}, \
                 minor version {minor}"
            ))),
        }
    }
}

#[cfg(test)]
mod test {
    use strum::IntoEnumIterator;

    use super::*;
    use std::str::FromStr;

    #[test]
    fn no_underscores_in_family_aliases() {
        for family in Family::iter() {
            let alias = family.as_str();
            assert!(
                !alias.contains('_'),
                "variant {:?}, alias {}",
                family,
                alias
            );
        }
    }

    #[test]
    fn platforms_round_trip_through_strings() {
        for platform in VirtualPlatform::iter() {
            let alias = format!("{platform}");
            let from_alias = VirtualPlatform::from_str(&alias).unwrap();
            assert_eq!(platform, from_alias);
        }
    }
}
