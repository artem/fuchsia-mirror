// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::operations::size_check::diff::{BlobDiff, PackageDiff, PackageReferenceDiff, SizeDiff};
use assembly_manifest::BlobfsContents;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};

/// These are resources supplied by the platform that the product does not need to be specifically
/// aware of. We will calculate the space consumed by these resources and subtract that space from
/// the total space allowed in blobfs.
const RESOURCES: &'static [&'static str] = &[
    "lib/VkLayer_image_pipe_swapchain.so",
    "lib/ld.so.1",
    "lib/libasync-default.so",
    "lib/libfdio.so",
    "lib/libsyslog.so",
    "lib/libtrace-engine.so",
    "lib/libtrace-provider-so.so",
    "lib/libvulkan.so",
    "lib/libc++.so.2",
    "lib/libc++abi.so.1",
    "lib/libunwind.so.1",
    "lib/libre2.so",
];

#[derive(Clone)]
pub struct SizeBreakdown {
    /// Map of package name to breakdown.
    pub packages: BTreeMap<String, PackageBreakdown>,
    /// Map of blob merkle to breakdown.
    pub blobs: BTreeMap<String, BlobBreakdown>,
}

/// The result of a size calculation.
#[derive(Debug, PartialEq)]
pub struct SizeResult {
    /// How many bytes are used up including resources.
    pub consumed_bytes: u64,
    /// How many bytes are used up only by resources.
    pub consumed_bytes_resources: u64,
}

impl SizeBreakdown {
    pub fn print(&self) {
        println!("{}", self.get_print_lines().join("\n"));
    }

    pub fn calculate_size(&self) -> SizeResult {
        let resources: HashSet<String> =
            HashSet::from_iter(RESOURCES.iter().map(|s| s.to_string()));
        let consumed_bytes = self.blobs.values().map(|b| b.size).sum();
        let consumed_bytes_resources = self
            .blobs
            .values()
            .filter_map(|b| if b.is_resource(&resources) { Some(b.size) } else { None })
            .sum();
        SizeResult { consumed_bytes, consumed_bytes_resources }
    }

    pub fn get_print_lines(&self) -> Vec<String> {
        let mut lines = vec![];
        lines.push(format!(
            "{: <43}{: >10}{: >10}{: >10}{: >10}",
            "PACKAGES", "MERKLE", "SIZE", "PSIZE", "SHARE"
        ));
        for (name, package) in &self.packages {
            let size: u64 = package.blobs.values().map(|b| b.size).sum();
            let psize: u64 = package.blobs.values().map(|b| b.psize).sum();
            lines.push(format!("{: <53}{: >10}{: >10}", name, size, psize));
            for (path, blob) in &package.blobs {
                lines.push(format!(
                    "{: <3}{: <40}{: >10}{: >10}{: >10}{: >10}",
                    "",
                    wrap_path(3, 40, &path),
                    &blob.hash[..6],
                    blob.size,
                    blob.psize,
                    blob.references.len(),
                ));
            }
        }
        lines
    }
}

fn wrap_path(indent: usize, length: usize, path: &String) -> String {
    if path.len() <= length {
        return path.clone();
    }

    let mut wrapped = String::new();

    // Add the first line.
    let first_length = length - 3;
    wrapped += &format!("{}...\n", &path[..first_length]);

    // Add the following lines.
    let following_length = length - 6;
    let following_chars = path[first_length..].chars().collect::<Vec<char>>();
    let following = following_chars
        .chunks(following_length)
        .map(|c| c.iter().collect::<String>())
        .map(|s| format!("{:indent$}   {}", "", s, indent = indent))
        .collect::<Vec<String>>();
    wrapped += &following.join("...\n");

    // Add any remaining spaces to line back up to length.
    let remaining = length + indent - following.last().map(|s| s.len()).unwrap_or(0);
    wrapped += &format!("{:r$}", "", r = remaining);
    wrapped
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct PackageBreakdown {
    /// Name of the package.
    pub name: String,
    /// Map of blob path in the package to breakdown.
    pub blobs: BTreeMap<String, BlobBreakdown>,
}

impl PackageBreakdown {
    fn into_added_diff(self, new_blobs: &HashSet<String>) -> PackageDiff {
        PackageDiff::added(
            self.name,
            self.blobs
                .iter()
                .filter_map(|(_, b)| if new_blobs.contains(&b.hash) { Some(b.size) } else { None })
                .sum(),
        )
    }

    fn into_removed_diff(self, old_blobs: &HashSet<String>) -> PackageDiff {
        PackageDiff::removed(
            self.name,
            self.blobs
                .iter()
                .filter_map(|(_, b)| if old_blobs.contains(&b.hash) { Some(b.size) } else { None })
                .sum(),
        )
    }

    fn diff(&self, before: &Self) -> PackageDiff {
        let size: u64 = self.blobs.values().map(|b| b.size).sum();
        let before_size: u64 = before.blobs.values().map(|b| b.size).sum();
        PackageDiff::updated(self.name.clone(), size, size as i64 - before_size as i64)
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct BlobBreakdown {
    pub hash: String,
    pub size: u64,
    pub psize: u64,
    pub references: Vec<PackageReference>,
}

impl BlobBreakdown {
    fn into_added_diff(self) -> BlobDiff {
        let Self { hash, size, psize, references } = self;
        let references =
            references.into_iter().map(|r| PackageReferenceDiff::added(r.name, r.path)).collect();
        BlobDiff::added(hash, size, psize, references)
    }

    fn into_removed_diff(self) -> BlobDiff {
        let Self { hash, size, psize, references } = self;
        let references =
            references.into_iter().map(|r| PackageReferenceDiff::removed(r.name, r.path)).collect();
        BlobDiff::removed(hash, size, psize, references)
    }

    fn into_unchanged_diff(self) -> BlobDiff {
        let Self { hash, size, psize, references } = self;
        let references =
            references.into_iter().map(|r| PackageReferenceDiff::updated(r.name, r.path)).collect();
        BlobDiff::unchanged(hash, size, psize, references)
    }

    fn is_resource(&self, resources: &HashSet<String>) -> bool {
        for reference in &self.references {
            if resources.contains(&reference.path) {
                return true;
            }
        }
        return false;
    }

    fn diff(&self, before: &Self) -> BlobDiff {
        let size_delta = self.size as i64 - before.size as i64;
        let psize_delta = self.psize as i64 - before.psize as i64;

        let before_references: HashSet<PackageReference> =
            HashSet::from_iter(before.references.clone().into_iter());
        let after_references: HashSet<PackageReference> =
            HashSet::from_iter(self.references.clone().into_iter());
        let references_added: Vec<PackageReferenceDiff> = after_references
            .difference(&before_references)
            .cloned()
            .map(|r| PackageReferenceDiff::added(r.name, r.path))
            .collect();
        let references_removed: Vec<PackageReferenceDiff> = before_references
            .difference(&after_references)
            .cloned()
            .map(|r| PackageReferenceDiff::removed(r.name, r.path))
            .collect();
        let references_updated: Vec<PackageReferenceDiff> = before_references
            .intersection(&after_references)
            .cloned()
            .map(|r| PackageReferenceDiff::updated(r.name, r.path))
            .collect();
        let mut references = Vec::new();
        references.extend(references_added);
        references.extend(references_removed);
        references.extend(references_updated);

        BlobDiff::updated(
            self.hash.clone(),
            before.hash.clone(),
            self.size,
            size_delta,
            self.psize,
            psize_delta,
            references,
        )
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct PackageReference {
    pub name: String,
    pub path: String,
}

impl PartialOrd for PackageReference {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PackageReference {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.name == other.name {
            return self.path.cmp(&other.path);
        }
        self.name.cmp(&other.name)
    }
}

impl SizeBreakdown {
    pub fn from_contents(contents: &BlobfsContents) -> Self {
        let mut blobs = BTreeMap::new();
        for package in contents.packages.base.0.iter().chain(contents.packages.cache.0.iter()) {
            for blob in &package.blobs {
                let entry = blobs.entry(blob.merkle.clone()).or_insert(BlobBreakdown {
                    size: blob.used_space_in_blobfs,
                    psize: 0,
                    hash: blob.merkle.clone(),
                    references: vec![],
                });
                entry
                    .references
                    .push(PackageReference { name: package.name.clone(), path: blob.path.clone() });
            }
        }
        for (_, blob) in &mut blobs {
            let count: u64 = blob.references.len().try_into().unwrap();
            blob.psize = blob.size / count;
        }
        let mut packages = BTreeMap::new();
        for (_, blob) in &blobs {
            for reference in &blob.references {
                let entry = packages.entry(reference.name.clone()).or_insert(PackageBreakdown {
                    name: reference.name.clone(),
                    blobs: BTreeMap::new(),
                });
                entry.blobs.insert(reference.path.clone(), blob.clone());
            }
        }
        Self { packages, blobs }
    }

    pub fn diff(&self, before: &Self) -> SizeDiff {
        let mut diff = SizeDiff::default();

        // Add the blob diff for updated or removed blobs.
        let mut added_blobs = HashSet::new();
        let mut removed_blobs = HashSet::new();
        let mut updated_blobs = HashSet::new();
        let mut unchanged_blobs = HashSet::new();
        for (hash, before_blob) in before.blobs.iter() {
            let after_blob = self.blobs.get(hash);

            // The blob is identical before and after, so skip.
            if Some(before_blob) == after_blob {
                unchanged_blobs.insert(hash.clone());
                diff.blob(before_blob.clone().into_unchanged_diff());
                continue;
            }

            // The blob has the same merkle before and after, but something else has changed.
            if let Some(after_blob) = after_blob {
                updated_blobs.insert(hash.clone());
                diff.blob(after_blob.diff(before_blob));
                continue;
            }

            // The blob has been modified and has a new merkle, but still exists in one of the same
            // packages.
            let after_blob =
                before_blob.references.iter().find_map(|r| self.find_blob_by_reference(&r));
            if let Some(after_blob) = after_blob {
                updated_blobs.insert(after_blob.hash.clone());
                diff.blob(after_blob.diff(before_blob));
                continue;
            }

            // Otherwise, assume the blob has been removed.
            removed_blobs.insert(hash.clone());
            diff.blob(before_blob.clone().into_removed_diff());
        }

        // Add the blob diffs for new blobs.
        for (hash, after_blob) in self.blobs.iter() {
            if !unchanged_blobs.contains(hash)
                && !updated_blobs.contains(hash)
                && !removed_blobs.contains(hash)
            {
                added_blobs.insert(hash.clone());
                diff.blob(after_blob.clone().into_added_diff());
            }
        }

        // Add the package that have been added or removed.
        for (name, package) in &self.packages {
            if !before.packages.contains_key(name) {
                diff.package(package.clone().into_added_diff(&added_blobs));
            }
        }
        for (name, package) in &before.packages {
            if !self.packages.contains_key(name) {
                diff.package(package.clone().into_removed_diff(&removed_blobs));
            }
        }
        for (name, package) in &self.packages {
            if let Some(before_package) = before.packages.get(name) {
                diff.package(package.diff(before_package));
            }
        }
        diff
    }

    // Find the first blob referenced in 'packages' using 'reference'.
    fn find_blob_by_reference<'a>(
        &'a self,
        reference: &PackageReference,
    ) -> Option<&'a BlobBreakdown> {
        self.packages
            .get(&reference.name)
            .map(|p| {
                p.blobs.iter().find(|(path, _)| **path == reference.path).map(|(_, blob)| blob)
            })
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn test_wrap_path() {
        let path = "abcdefghijklmnopqrstuvwxyz".to_string();
        let expected_path = r"abcdefg...
      hijk...
      lmno...
      pqrs...
      tuvw...
      xyz    ";
        assert_eq!(expected_path, wrap_path(3, 10, &path));
    }

    #[test]
    fn test_calculate_size() {
        let breakdown = SizeBreakdown {
            // total size calculation does not use packages.
            packages: BTreeMap::new(),
            blobs: BTreeMap::from([
                (
                    "lib/lib1".to_string(),
                    BlobBreakdown {
                        hash: "abcdefghij".to_string(),
                        size: 10,
                        psize: 10,
                        references: vec![PackageReference {
                            path: "lib/lib1".to_string(),
                            name: "pkg-one".to_string(),
                        }],
                    },
                ),
                (
                    "lib/lib2".to_string(),
                    BlobBreakdown {
                        hash: "1234567890".to_string(),
                        size: 30,
                        psize: 15,
                        references: vec![
                            PackageReference {
                                path: "lib/lib2".to_string(),
                                name: "pkg-one".to_string(),
                            },
                            PackageReference {
                                path: "lib/lib2".to_string(),
                                name: "pkg-two".to_string(),
                            },
                        ],
                    },
                ),
                (
                    "lib/lib3".to_string(),
                    BlobBreakdown {
                        hash: "0987654321".to_string(),
                        size: 5,
                        psize: 5,
                        references: vec![PackageReference {
                            path: "lib/lib3".to_string(),
                            name: "pkg-one".to_string(),
                        }],
                    },
                ),
                // Add a resource that should be added to the resources group.
                (
                    "lib/ld.so.1".to_string(),
                    BlobBreakdown {
                        hash: "defghijklm".to_string(),
                        size: 10,
                        psize: 10,
                        references: vec![PackageReference {
                            path: "lib/ld.so.1".to_string(),
                            name: "pkg-one".to_string(),
                        }],
                    },
                ),
            ]),
        };
        let expected_total = SizeResult { consumed_bytes: 55, consumed_bytes_resources: 10 };
        assert_eq!(expected_total, breakdown.calculate_size());
    }

    #[test]
    fn test_print() {
        let breakdown = SizeBreakdown {
            packages: BTreeMap::from([
                // We put these out-of-order to ensure the output is sorted.
                (
                    "pkg-two".to_string(),
                    PackageBreakdown {
                        name: "pkg-two".to_string(),
                        blobs: BTreeMap::from([
                            (
                                "lib/lib3".to_string(),
                                BlobBreakdown {
                                    hash: "0987654321".to_string(),
                                    size: 5,
                                    psize: 5,
                                    references: vec![PackageReference {
                                        path: "lib/lib3".to_string(),
                                        name: "pkg-one".to_string(),
                                    }],
                                },
                            ),
                            (
                                "lib/lib2".to_string(),
                                BlobBreakdown {
                                    hash: "1234567890".to_string(),
                                    size: 30,
                                    psize: 15,
                                    references: vec![
                                        PackageReference {
                                            path: "lib/lib2".to_string(),
                                            name: "pkg-one".to_string(),
                                        },
                                        PackageReference {
                                            path: "lib/lib2".to_string(),
                                            name: "pkg-two".to_string(),
                                        },
                                    ],
                                },
                            ),
                        ]),
                    },
                ),
                (
                    "pkg-one".to_string(),
                    PackageBreakdown {
                        name: "pkg-one".to_string(),
                        blobs: BTreeMap::from([(
                            "lib/lib1".to_string(),
                            BlobBreakdown {
                                hash: "abcdefghij".to_string(),
                                size: 10,
                                psize: 10,
                                references: vec![PackageReference {
                                    path: "lib/lib1".to_string(),
                                    name: "pkg-one".to_string(),
                                }],
                            },
                        )]),
                    },
                ),
            ]),
            // printing does not use the blobs field.
            blobs: BTreeMap::new(),
        };
        let lines = breakdown.get_print_lines();
        let expected_lines: Vec<String> = vec![
            "PACKAGES                                       MERKLE      SIZE     PSIZE     SHARE",
            "pkg-one                                                      10        10",
            "   lib/lib1                                    abcdef        10        10         1",
            "pkg-two                                                      35        20",
            "   lib/lib2                                    123456        30        15         2",
            "   lib/lib3                                    098765         5         5         1",
        ]
        .into_iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(expected_lines, lines);
    }

    #[test]
    fn test_diff_same_empty() {
        let before = SizeBreakdown { packages: BTreeMap::new(), blobs: BTreeMap::new() };
        let after = before.clone();
        let diff = after.diff(&before);
        assert_eq!(SizeDiff::default(), diff);
    }

    #[test]
    fn test_diff_same() {
        let before = SizeBreakdown {
            packages: BTreeMap::from([
                (
                    "pkg-one".to_string(),
                    PackageBreakdown {
                        name: "pkg-one".to_string(),
                        blobs: BTreeMap::from([(
                            "lib/lib1".to_string(),
                            BlobBreakdown {
                                hash: "abcdefghij".to_string(),
                                size: 10,
                                psize: 10,
                                references: vec![PackageReference {
                                    path: "lib/lib1".to_string(),
                                    name: "pkg-one".to_string(),
                                }],
                            },
                        )]),
                    },
                ),
                (
                    "pkg-two".to_string(),
                    PackageBreakdown {
                        name: "pkg-two".to_string(),
                        blobs: BTreeMap::from([(
                            "lib/lib2".to_string(),
                            BlobBreakdown {
                                hash: "1234567890".to_string(),
                                size: 30,
                                psize: 15,
                                references: vec![
                                    PackageReference {
                                        path: "lib/lib2".to_string(),
                                        name: "pkg-one".to_string(),
                                    },
                                    PackageReference {
                                        path: "lib/lib2".to_string(),
                                        name: "pkg-two".to_string(),
                                    },
                                ],
                            },
                        )]),
                    },
                ),
            ]),
            blobs: BTreeMap::from([
                (
                    "abcdefghij".to_string(),
                    BlobBreakdown {
                        hash: "abcdefghij".to_string(),
                        size: 10,
                        psize: 10,
                        references: vec![PackageReference {
                            path: "lib/lib1".to_string(),
                            name: "pkg-one".to_string(),
                        }],
                    },
                ),
                (
                    "1234567890".to_string(),
                    BlobBreakdown {
                        hash: "1234567890".to_string(),
                        size: 30,
                        psize: 15,
                        references: vec![PackageReference {
                            path: "lib/lib2".to_string(),
                            name: "pkg-two".to_string(),
                        }],
                    },
                ),
            ]),
        };
        let after = before.clone();
        let diff = after.diff(&before);

        let mut expected_diff = SizeDiff::default();
        expected_diff.package(PackageDiff::updated("pkg-one".to_string(), 10, 0));
        expected_diff.package(PackageDiff::updated("pkg-two".to_string(), 30, 0));
        expected_diff.blob(BlobDiff::unchanged(
            "1234567890".to_string(),
            30,
            15,
            vec![PackageReferenceDiff::updated("pkg-two".to_string(), "lib/lib2".to_string())],
        ));
        expected_diff.blob(BlobDiff::unchanged(
            "abcdefghij".to_string(),
            10,
            10,
            vec![PackageReferenceDiff::updated("pkg-one".to_string(), "lib/lib1".to_string())],
        ));
        assert_eq!(expected_diff, diff);
    }

    #[test]
    fn test_diff() {
        let before = SizeBreakdown {
            packages: BTreeMap::from([
                // This package will still be present in after.
                (
                    "pkg-one".to_string(),
                    PackageBreakdown {
                        name: "pkg-one".to_string(),
                        blobs: BTreeMap::from([
                            // This blob will be unchanged.
                            (
                                "lib/unchanged".to_string(),
                                BlobBreakdown {
                                    hash: "unchanged_blob".to_string(),
                                    size: 10,
                                    psize: 10,
                                    references: vec![PackageReference {
                                        path: "lib/unchanged".to_string(),
                                        name: "pkg-one".to_string(),
                                    }],
                                },
                            ),
                            // This blob will be removed.
                            (
                                "lib/removed".to_string(),
                                BlobBreakdown {
                                    hash: "removed_blob".to_string(),
                                    size: 20,
                                    psize: 20,
                                    references: vec![PackageReference {
                                        path: "lib/removed".to_string(),
                                        name: "pkg-one".to_string(),
                                    }],
                                },
                            ),
                            // This blob will be shared one more time.
                            (
                                "lib/updated_share".to_string(),
                                BlobBreakdown {
                                    hash: "updated_share".to_string(),
                                    size: 30,
                                    psize: 30,
                                    references: vec![PackageReference {
                                        path: "lib/updated_share".to_string(),
                                        name: "pkg-one".to_string(),
                                    }],
                                },
                            ),
                            // This blob will increase in size and get a new merkle.
                            (
                                "lib/updated_merkle".to_string(),
                                BlobBreakdown {
                                    hash: "updated_merkle_old".to_string(),
                                    size: 40,
                                    psize: 40,
                                    references: vec![PackageReference {
                                        path: "lib/updated_merkle".to_string(),
                                        name: "pkg-one".to_string(),
                                    }],
                                },
                            ),
                        ]),
                    },
                ),
                // This package should be removed.
                (
                    "pkg-two".to_string(),
                    PackageBreakdown {
                        name: "pkg-two".to_string(),
                        blobs: BTreeMap::from([(
                            "lib/lib2".to_string(),
                            BlobBreakdown {
                                hash: "1234567890".to_string(),
                                size: 30,
                                psize: 15,
                                references: vec![PackageReference {
                                    path: "lib/lib2".to_string(),
                                    name: "pkg-two".to_string(),
                                }],
                            },
                        )]),
                    },
                ),
            ]),
            blobs: BTreeMap::from([
                (
                    "unchanged_blob".to_string(),
                    BlobBreakdown {
                        hash: "unchanged_blob".to_string(),
                        size: 10,
                        psize: 10,
                        references: vec![PackageReference {
                            path: "lib/unchanged".to_string(),
                            name: "pkg-one".to_string(),
                        }],
                    },
                ),
                (
                    "removed_blob".to_string(),
                    BlobBreakdown {
                        hash: "removed_blob".to_string(),
                        size: 20,
                        psize: 20,
                        references: vec![PackageReference {
                            path: "lib/removed".to_string(),
                            name: "pkg-one".to_string(),
                        }],
                    },
                ),
                (
                    "updated_share".to_string(),
                    BlobBreakdown {
                        hash: "updated_share".to_string(),
                        size: 30,
                        psize: 30,
                        references: vec![PackageReference {
                            path: "lib/updated_share".to_string(),
                            name: "pkg-one".to_string(),
                        }],
                    },
                ),
                (
                    "updated_merkle".to_string(),
                    BlobBreakdown {
                        hash: "updated_merkle_old".to_string(),
                        size: 40,
                        psize: 40,
                        references: vec![PackageReference {
                            path: "lib/updated_merkle".to_string(),
                            name: "pkg-one".to_string(),
                        }],
                    },
                ),
                (
                    "1234567890".to_string(),
                    BlobBreakdown {
                        hash: "1234567890".to_string(),
                        size: 30,
                        psize: 15,
                        references: vec![PackageReference {
                            path: "lib/lib2".to_string(),
                            name: "pkg-two".to_string(),
                        }],
                    },
                ),
            ]),
        };
        let after = SizeBreakdown {
            packages: BTreeMap::from([
                (
                    "pkg-one".to_string(),
                    PackageBreakdown {
                        name: "pkg-one".to_string(),
                        blobs: BTreeMap::from([
                            // This blob was unchanged.
                            (
                                "lib/lib1".to_string(),
                                BlobBreakdown {
                                    hash: "abcdefghij".to_string(),
                                    size: 10,
                                    psize: 10,
                                    references: vec![PackageReference {
                                        path: "lib/lib1".to_string(),
                                        name: "pkg-one".to_string(),
                                    }],
                                },
                            ),
                            // This blob was added.
                            (
                                "lib/added".to_string(),
                                BlobBreakdown {
                                    hash: "added_blob".to_string(),
                                    size: 20,
                                    psize: 20,
                                    references: vec![PackageReference {
                                        path: "lib/added".to_string(),
                                        name: "pkg-one".to_string(),
                                    }],
                                },
                            ),
                            // This blob was shared one more time.
                            (
                                "lib/updated_share".to_string(),
                                BlobBreakdown {
                                    hash: "updated_share".to_string(),
                                    size: 30,
                                    psize: 15,
                                    references: vec![
                                        PackageReference {
                                            path: "lib/updated_share".to_string(),
                                            name: "pkg-two".to_string(),
                                        },
                                        PackageReference {
                                            path: "lib/updated_share".to_string(),
                                            name: "pkg-two".to_string(),
                                        },
                                    ],
                                },
                            ),
                            // This blob was increased in size and got a new merkle.
                            (
                                "lib/updated_merkle".to_string(),
                                BlobBreakdown {
                                    hash: "updated_merkle_new".to_string(),
                                    size: 50,
                                    psize: 50,
                                    references: vec![PackageReference {
                                        path: "lib/updated_merkle".to_string(),
                                        name: "pkg-one".to_string(),
                                    }],
                                },
                            ),
                        ]),
                    },
                ),
                // This package was added, and includes a blob that was shared from pkg-one.
                (
                    "pkg-three".to_string(),
                    PackageBreakdown {
                        name: "pkg-three".to_string(),
                        blobs: BTreeMap::from([
                            (
                                "lib/updated_share".to_string(),
                                BlobBreakdown {
                                    hash: "updated_share".to_string(),
                                    size: 30,
                                    psize: 15,
                                    references: vec![
                                        PackageReference {
                                            path: "lib/updated_share".to_string(),
                                            name: "pkg-three".to_string(),
                                        },
                                        PackageReference {
                                            path: "lib/updated_share".to_string(),
                                            name: "pkg-three".to_string(),
                                        },
                                    ],
                                },
                            ),
                            (
                                "lib/lib2".to_string(),
                                BlobBreakdown {
                                    hash: "0987654321".to_string(),
                                    size: 60,
                                    psize: 60,
                                    references: vec![PackageReference {
                                        path: "lib/lib2".to_string(),
                                        name: "pkg-three".to_string(),
                                    }],
                                },
                            ),
                        ]),
                    },
                ),
            ]),
            blobs: BTreeMap::from([
                (
                    "unchanged_blob".to_string(),
                    BlobBreakdown {
                        hash: "unchanged_blob".to_string(),
                        size: 10,
                        psize: 10,
                        references: vec![PackageReference {
                            path: "lib/unchanged".to_string(),
                            name: "pkg-one".to_string(),
                        }],
                    },
                ),
                (
                    "added_blob".to_string(),
                    BlobBreakdown {
                        hash: "added_blob".to_string(),
                        size: 20,
                        psize: 20,
                        references: vec![PackageReference {
                            path: "lib/added".to_string(),
                            name: "pkg-one".to_string(),
                        }],
                    },
                ),
                (
                    "updated_share".to_string(),
                    BlobBreakdown {
                        hash: "updated_share".to_string(),
                        size: 30,
                        psize: 15,
                        references: vec![
                            PackageReference {
                                path: "lib/updated_share".to_string(),
                                name: "pkg-one".to_string(),
                            },
                            PackageReference {
                                path: "lib/updated_share".to_string(),
                                name: "pkg-three".to_string(),
                            },
                        ],
                    },
                ),
                (
                    "updated_merkle".to_string(),
                    BlobBreakdown {
                        hash: "updated_merkle_new".to_string(),
                        size: 50,
                        psize: 50,
                        references: vec![PackageReference {
                            path: "lib/updated_merkle".to_string(),
                            name: "pkg-one".to_string(),
                        }],
                    },
                ),
                (
                    "0987654321".to_string(),
                    BlobBreakdown {
                        hash: "0987654321".to_string(),
                        size: 60,
                        psize: 60,
                        references: vec![PackageReference {
                            path: "lib/lib2".to_string(),
                            name: "pkg-three".to_string(),
                        }],
                    },
                ),
            ]),
        };
        let diff = after.diff(&before);

        let mut expected_diff = SizeDiff::default();
        expected_diff.package(PackageDiff::added("pkg-three".to_string(), 60));
        expected_diff.package(PackageDiff::removed("pkg-two".to_string(), 30));
        expected_diff.package(PackageDiff::updated("pkg-one".to_string(), 110, 10));
        expected_diff.blob(BlobDiff::added(
            "0987654321".to_string(),
            60,
            60,
            vec![PackageReferenceDiff::added("pkg-three".to_string(), "lib/lib2".to_string())],
        ));
        expected_diff.blob(BlobDiff::added(
            "added_blob".to_string(),
            20,
            20,
            vec![PackageReferenceDiff::added("pkg-one".to_string(), "lib/added".to_string())],
        ));
        expected_diff.blob(BlobDiff::removed(
            "1234567890".to_string(),
            30,
            15,
            vec![PackageReferenceDiff::removed("pkg-two".to_string(), "lib/lib2".to_string())],
        ));
        expected_diff.blob(BlobDiff::removed(
            "removed_blob".to_string(),
            20,
            20,
            vec![PackageReferenceDiff::removed("pkg-one".to_string(), "lib/removed".to_string())],
        ));
        expected_diff.blob(BlobDiff::updated(
            "updated_share".to_string(),
            "updated_share".to_string(),
            30,
            0,
            15,
            -15,
            vec![
                PackageReferenceDiff::added(
                    "pkg-three".to_string(),
                    "lib/updated_share".to_string(),
                ),
                PackageReferenceDiff::updated(
                    "pkg-one".to_string(),
                    "lib/updated_share".to_string(),
                ),
            ],
        ));
        expected_diff.blob(BlobDiff::updated(
            "updated_merkle_new".to_string(),
            "updated_merkle_old".to_string(),
            50,
            10,
            50,
            10,
            vec![PackageReferenceDiff::updated(
                "pkg-one".to_string(),
                "lib/updated_merkle".to_string(),
            )],
        ));
        expected_diff.blob(BlobDiff::unchanged(
            "unchanged_blob".to_string(),
            10,
            10,
            vec![PackageReferenceDiff::updated("pkg-one".to_string(), "lib/unchanged".to_string())],
        ));
        assert_eq!(expected_diff, diff);
    }
}
