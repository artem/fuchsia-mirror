// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    cm_types::{LongName, Name},
    core::cmp::Ordering,
    moniker::{ChildName, ChildNameBase, MonikerError},
    std::fmt,
};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// An instanced child moniker locally identifies a child component instance using the name assigned by
/// its parent and its collection (if present). It is a building block for more complex monikers.
///
/// Display notation: "[collection:]name:instance_id".
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Eq, PartialEq, Clone, Hash)]
pub struct InstancedChildName {
    name: LongName,
    collection: Option<Name>,
    instance: IncarnationId,
}

pub type IncarnationId = u32;

impl ChildNameBase for InstancedChildName {
    /// Parses an `ChildName` from a string.
    ///
    /// Input strings should be of the format `(<collection>:)?<name>:<instance_id>`, e.g. `foo:42`
    /// or `coll:foo:42`.
    fn parse<T: AsRef<str>>(rep: T) -> Result<Self, MonikerError> {
        let rep = rep.as_ref();
        let parts: Vec<&str> = rep.split(":").collect();
        // An instanced moniker is either just a name (static instance), or
        // collection:name:instance_id.
        let (coll, name, instance) = match parts.len() {
            2 => {
                let instance = parts[1]
                    .parse::<IncarnationId>()
                    .map_err(|_| MonikerError::invalid_moniker(rep))?;
                (None, parts[0], instance)
            }
            3 => {
                let instance = parts[2]
                    .parse::<IncarnationId>()
                    .map_err(|_| MonikerError::invalid_moniker(rep))?;
                (Some(parts[0]), parts[1], instance)
            }
            _ => return Err(MonikerError::invalid_moniker(rep)),
        };
        Self::try_new(name, coll, instance)
    }

    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn collection(&self) -> Option<&Name> {
        self.collection.as_ref()
    }
}

impl InstancedChildName {
    pub fn try_new<S>(
        name: S,
        collection: Option<S>,
        instance: IncarnationId,
    ) -> Result<Self, MonikerError>
    where
        S: AsRef<str> + Into<String>,
    {
        let name = LongName::new(name)?;
        let collection = match collection {
            Some(coll) => {
                let coll_name = Name::new(coll)?;
                Some(coll_name)
            }
            None => None,
        };
        Ok(Self { name, collection, instance })
    }

    /// Returns a moniker for a static child.
    ///
    /// The returned value will have no `collection`, and will have an `instance_id` of 0.
    pub fn static_child(name: &str) -> Result<Self, MonikerError> {
        Self::try_new(name, None, 0)
    }

    /// Converts this child moniker into an instanced moniker.
    pub fn from_child_moniker(m: &ChildName, instance: IncarnationId) -> Self {
        Self::try_new(m.name(), m.collection().map(|c| c.as_str()), instance)
            .expect("child moniker is guaranteed to be valid")
    }

    /// Convert an InstancedChildName to an allocated ChildName
    /// without an InstanceId
    pub fn without_instance_id(&self) -> ChildName {
        ChildName::try_new(self.name(), self.collection().map(|c| c.as_str()))
            .expect("moniker is guaranteed to be valid")
    }

    pub fn instance(&self) -> IncarnationId {
        self.instance
    }

    pub fn format(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(coll) = &self.collection {
            write!(f, "{}:{}:{}", coll, self.name, self.instance)
        } else {
            write!(f, "{}:{}", self.name, self.instance)
        }
    }
}

impl TryFrom<&str> for InstancedChildName {
    type Error = MonikerError;

    fn try_from(rep: &str) -> Result<Self, MonikerError> {
        InstancedChildName::parse(rep)
    }
}

impl Ord for InstancedChildName {
    fn cmp(&self, other: &Self) -> Ordering {
        (&self.collection, &self.name, &self.instance).cmp(&(
            &other.collection,
            &other.name,
            &other.instance,
        ))
    }
}

impl PartialOrd for InstancedChildName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for InstancedChildName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.format(f)
    }
}

impl fmt::Debug for InstancedChildName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.format(f)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        cm_types::{MAX_LONG_NAME_LENGTH, MAX_NAME_LENGTH},
    };

    #[test]
    fn instanced_child_monikers() {
        let m = InstancedChildName::try_new("test", None, 42).unwrap();
        assert_eq!("test", m.name());
        assert_eq!(None, m.collection());
        assert_eq!(42, m.instance());
        assert_eq!("test:42", format!("{}", m));
        assert_eq!(m, InstancedChildName::try_from("test:42").unwrap());
        assert_eq!("test", m.without_instance_id().to_string());
        assert_eq!(m, InstancedChildName::from_child_moniker(&"test".try_into().unwrap(), 42));

        let m = InstancedChildName::try_new("test", Some("coll"), 42).unwrap();
        assert_eq!("test", m.name());
        assert_eq!(Some(&Name::new("coll").unwrap()), m.collection());
        assert_eq!(42, m.instance());
        assert_eq!("coll:test:42", format!("{}", m));
        assert_eq!(m, InstancedChildName::try_from("coll:test:42").unwrap());
        assert_eq!("coll:test", m.without_instance_id().to_string());
        assert_eq!(m, InstancedChildName::from_child_moniker(&"coll:test".try_into().unwrap(), 42));

        let max_coll_length_part = "f".repeat(MAX_NAME_LENGTH);
        let max_name_length_part: LongName = "f".repeat(MAX_LONG_NAME_LENGTH).parse().unwrap();
        let m = InstancedChildName::parse(format!(
            "{}:{}:42",
            max_coll_length_part, max_name_length_part
        ))
        .expect("valid moniker");
        assert_eq!(max_name_length_part, m.name());
        assert_eq!(Some(&Name::new(max_coll_length_part).unwrap()), m.collection());
        assert_eq!(42, m.instance());

        assert!(InstancedChildName::parse("").is_err(), "cannot be empty");
        assert!(InstancedChildName::parse(":").is_err(), "cannot be empty with colon");
        assert!(InstancedChildName::parse("::").is_err(), "cannot be empty with double colon");
        assert!(InstancedChildName::parse("f:").is_err(), "second part cannot be empty with colon");
        assert!(InstancedChildName::parse(":1").is_err(), "first part cannot be empty with colon");
        assert!(
            InstancedChildName::parse("f:f:").is_err(),
            "third part cannot be empty with colon"
        );
        assert!(
            InstancedChildName::parse("f::1").is_err(),
            "second part cannot be empty with colon"
        );
        assert!(
            InstancedChildName::parse(":f:1").is_err(),
            "first part cannot be empty with colon"
        );
        assert!(
            InstancedChildName::parse("f:f:1:1").is_err(),
            "more than three colons not allowed"
        );
        assert!(InstancedChildName::parse("f:f").is_err(), "second part must be int");
        assert!(InstancedChildName::parse("f:f:f").is_err(), "third part must be int");
        assert!(InstancedChildName::parse("@:1").is_err(), "invalid character in name");
        assert!(InstancedChildName::parse("@:f:1").is_err(), "invalid character in collection");
        assert!(
            InstancedChildName::parse("f:@:1").is_err(),
            "invalid character in name with collection"
        );
        assert!(
            InstancedChildName::parse(&format!("f:{}", "x".repeat(MAX_LONG_NAME_LENGTH + 1)))
                .is_err(),
            "name too long"
        );
        assert!(
            InstancedChildName::parse(&format!("{}:x", "f".repeat(MAX_NAME_LENGTH + 1))).is_err(),
            "collection too long"
        );
    }

    #[test]
    fn instanced_child_moniker_compare() {
        let a = InstancedChildName::try_new("a", None, 1).unwrap();
        let a2 = InstancedChildName::try_new("a", None, 2).unwrap();
        let aa = InstancedChildName::try_new("a", Some("a"), 1).unwrap();
        let aa2 = InstancedChildName::try_new("a", Some("a"), 2).unwrap();
        let ab = InstancedChildName::try_new("a", Some("b"), 1).unwrap();
        let ba = InstancedChildName::try_new("b", Some("a"), 1).unwrap();
        let bb = InstancedChildName::try_new("b", Some("b"), 1).unwrap();
        let aa_same = InstancedChildName::try_new("a", Some("a"), 1).unwrap();

        assert_eq!(Ordering::Less, a.cmp(&a2));
        assert_eq!(Ordering::Greater, a2.cmp(&a));
        assert_eq!(Ordering::Less, a2.cmp(&aa));
        assert_eq!(Ordering::Greater, aa.cmp(&a2));
        assert_eq!(Ordering::Less, a.cmp(&ab));
        assert_eq!(Ordering::Greater, ab.cmp(&a));
        assert_eq!(Ordering::Less, a.cmp(&ba));
        assert_eq!(Ordering::Greater, ba.cmp(&a));
        assert_eq!(Ordering::Less, a.cmp(&bb));
        assert_eq!(Ordering::Greater, bb.cmp(&a));

        assert_eq!(Ordering::Less, aa.cmp(&aa2));
        assert_eq!(Ordering::Greater, aa2.cmp(&aa));
        assert_eq!(Ordering::Less, aa.cmp(&ab));
        assert_eq!(Ordering::Greater, ab.cmp(&aa));
        assert_eq!(Ordering::Less, aa.cmp(&ba));
        assert_eq!(Ordering::Greater, ba.cmp(&aa));
        assert_eq!(Ordering::Less, aa.cmp(&bb));
        assert_eq!(Ordering::Greater, bb.cmp(&aa));
        assert_eq!(Ordering::Equal, aa.cmp(&aa_same));
        assert_eq!(Ordering::Equal, aa_same.cmp(&aa));

        assert_eq!(Ordering::Greater, ab.cmp(&ba));
        assert_eq!(Ordering::Less, ba.cmp(&ab));
        assert_eq!(Ordering::Less, ab.cmp(&bb));
        assert_eq!(Ordering::Greater, bb.cmp(&ab));

        assert_eq!(Ordering::Less, ba.cmp(&bb));
        assert_eq!(Ordering::Greater, bb.cmp(&ba));
    }
}
