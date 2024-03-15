// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    lsm_tree::LayerInfo,
    object_store::{
        allocator::{AllocatorInfo, AllocatorKey, AllocatorValue},
        journal::{
            super_block::{
                SuperBlockHeader, SuperBlockRecord, SuperBlockRecordV32, SuperBlockRecordV33,
                SuperBlockRecordV37,
            },
            JournalRecord, JournalRecordV32, JournalRecordV33, JournalRecordV34, JournalRecordV36,
            JournalRecordV37,
        },
        object_record::{
            FsverityMetadata, ObjectKey, ObjectValue, ObjectValueV32, ObjectValueV33,
            ObjectValueV37,
        },
        transaction::{Mutation, MutationV32, MutationV33, MutationV37},
        EncryptedMutations, StoreInfo, StoreInfoV32,
    },
    serialized_types::{versioned_type, Version, Versioned, VersionedLatest},
};

/// The latest version of on-disk filesystem format.
///
/// If all layer files are compacted the the journal flushed, and super-block
/// both rewritten, all versions should match this value.
///
/// If making a breaking change, please see EARLIEST_SUPPORTED_VERSION (below).
///
/// IMPORTANT: When changing this (major or minor), update the list of possible versions at
/// https://cs.opensource.google/fuchsia/fuchsia/+/main:third_party/cobalt_config/fuchsia/local_storage/versions.txt.
pub const LATEST_VERSION: Version = Version { major: 38, minor: 0 };

// The version at which checksums for data were added to the journal as separate records. Once
// this version is the same or older than the earliest supported version, support for snooping the
// checksum from the extent record during replay can be removed.
pub const _JOURNAL_DATA_CHECKSUM_RECORD: Version = Version { major: 34, minor: 0 };

/// The earliest supported version of the on-disk filesystem format.
///
/// When a breaking change is made:
/// 1) LATEST_VERSION should have it's major component increased (see above).
/// 2) EARLIEST_SUPPORTED_VERSION should be set to the new LATEST_VERSION.
/// 3) The SuperBlockHeader version (below) should also be set to the new LATEST_VERSION.
///
/// Also check the constant version numbers above for any code cleanup that can happen.
pub const EARLIEST_SUPPORTED_VERSION: Version = Version { major: 32, minor: 0 };

versioned_type! {
    32.. => AllocatorInfo,
}
versioned_type! {
    32.. => AllocatorKey,
}
versioned_type! {
    32.. => AllocatorValue,
}
versioned_type! {
    32.. => EncryptedMutations,
}
versioned_type! {
    33.. => FsverityMetadata,
}
versioned_type! {
    38.. => JournalRecord,
    37.. => JournalRecordV37,
    36.. => JournalRecordV36,
    34.. => JournalRecordV34,
    33.. => JournalRecordV33,
    32.. => JournalRecordV32,
}
versioned_type! {
    32.. => LayerInfo,
}
versioned_type! {
    38.. => Mutation,
    37.. => MutationV37,
    33.. => MutationV33,
    32.. => MutationV32,
}
versioned_type! {
    32.. => ObjectKey,
}
versioned_type! {
    38.. => ObjectValue,
    37.. => ObjectValueV37,
    33.. => ObjectValueV33,
    32.. => ObjectValueV32,
}
versioned_type! {
    36.. => StoreInfo,
    32.. => StoreInfoV32,
}
versioned_type! {
    32.. => SuperBlockHeader,
}
versioned_type! {
    38.. => SuperBlockRecord,
    37.. => SuperBlockRecordV37,
    33.. => SuperBlockRecordV33,
    32.. => SuperBlockRecordV32,
}

#[cfg(test)]
mod tests {
    use crate::{
        lsm_tree::{simple_persistent_layer::LayerInfoV32, LayerInfo},
        object_store::{
            allocator::{
                AllocatorInfo, AllocatorInfoV32, AllocatorKey, AllocatorKeyV32, AllocatorValue,
                AllocatorValueV32,
            },
            journal::{
                super_block::{
                    SuperBlockHeader, SuperBlockHeaderV32, SuperBlockRecord, SuperBlockRecordV32,
                    SuperBlockRecordV33, SuperBlockRecordV37,
                },
                JournalRecord, JournalRecordV32, JournalRecordV33, JournalRecordV34,
                JournalRecordV36, JournalRecordV37,
            },
            object_record::{
                FsverityMetadataV33, ObjectKeyV32, ObjectValueV32, ObjectValueV33, ObjectValueV37,
            },
            transaction::{MutationV32, MutationV33, MutationV37},
            EncryptedMutations, EncryptedMutationsV32, FsverityMetadata, Mutation, ObjectKey,
            ObjectValue, StoreInfo, StoreInfoV32, StoreInfoV36,
        },
    };

    fn assert_type_fprint<T: fprint::TypeFingerprint>(fp: &str) -> bool {
        if T::fingerprint() != fp {
            eprintln!(
                "====> {} fingerprint changed to {}",
                std::any::type_name::<T>(),
                T::fingerprint()
            );
            false
        } else {
            true
        }
    }

    #[test]
    fn type_fprint_latest_version() {
        // These should only ever change when adding a new version.
        // The checks below are to ensure that we don't inadvertently change a serialized type.
        // Every versioned_type above should have a corresponding line entry here.
        let mut success = true;
        success &= assert_type_fprint::<AllocatorInfo>("struct {layers:Vec<u64>,allocated_bytes:BTreeMap<u64,u64>,marked_for_deletion:HashSet<u64>,limit_bytes:BTreeMap<u64,u64>}");
        success &= assert_type_fprint::<AllocatorKey>("struct {device_range:Range<u64>}");
        success &=
            assert_type_fprint::<AllocatorValue>("enum {None,Abs(count:u64,owner_object_id:u64)}");
        success &= assert_type_fprint::<EncryptedMutations>("struct {transactions:Vec<(struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},u64,)>,data:Vec<u8>,mutations_key_roll:Vec<(usize,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>}");
        success &= assert_type_fprint::<JournalRecord>("enum {EndBlock,Mutation(object_id:u64,mutation:enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64,has_overwrite_extents:bool),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,mode:enum {Raw,Cow(struct {sums:Vec<u8>}),OverwritePartial(bit_vec :: BitVec<u32>),Overwrite},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}}),CreateInternalDir(u64)}),Commit,Discard(u64),DidFlushDevice(u64),DataChecksums(Range<u64>,struct {sums:Vec<u8>})}");
        success &= assert_type_fprint::<LayerInfo>(
            "struct {key_value_version:struct {major:u32,minor:u8},block_size:u64}",
        );
        success &= assert_type_fprint::<FsverityMetadata>(
            "struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>}",
        );
        success &= assert_type_fprint::<Mutation>("enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64,has_overwrite_extents:bool),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,mode:enum {Raw,Cow(struct {sums:Vec<u8>}),OverwritePartial(bit_vec :: BitVec<u32>),Overwrite},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}}),CreateInternalDir(u64)}");
        success &= assert_type_fprint::<ObjectKey>("struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}}");
        success &= assert_type_fprint::<ObjectValue>("enum {None,Some,Object(kind:enum {File(refs:u64,has_overwrite_extents:bool),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,mode:enum {Raw,Cow(struct {sums:Vec<u8>}),OverwritePartial(bit_vec :: BitVec<u32>),Overwrite},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})}");
        success &= assert_type_fprint::<StoreInfo>("struct {guid:[u8;16],last_object_id:u64,layers:Vec<u64>,root_directory_object_id:u64,graveyard_directory_object_id:u64,object_count:u64,mutations_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>,mutations_cipher_offset:u64,encrypted_mutations_object_id:u64,object_id_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>,internal_directory_object_id:u64}");
        success &= assert_type_fprint::<SuperBlockHeader>("struct {guid:<[u8;16]>,generation:u64,root_parent_store_object_id:u64,root_parent_graveyard_directory_object_id:u64,root_store_object_id:u64,allocator_object_id:u64,journal_object_id:u64,journal_checkpoint:struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},super_block_journal_file_offset:u64,journal_file_offsets:HashMap<u64,u64>,borrowed_metadata_space:u64,earliest_version:struct {major:u32,minor:u8}}");
        success &= assert_type_fprint::<SuperBlockRecord>("enum {Extent(Range<u64>),ObjectItem(struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64,has_overwrite_extents:bool),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,mode:enum {Raw,Cow(struct {sums:Vec<u8>}),OverwritePartial(bit_vec :: BitVec<u32>),Overwrite},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64}),End}");
        assert!(success, "One or more versioned types have different type fingerprint.");
    }

    #[test]
    fn type_fprint_v37() {
        let mut success = true;
        success &= assert_type_fprint::<AllocatorInfoV32>("struct {layers:Vec<u64>,allocated_bytes:BTreeMap<u64,u64>,marked_for_deletion:HashSet<u64>,limit_bytes:BTreeMap<u64,u64>}");
        success &= assert_type_fprint::<AllocatorKeyV32>("struct {device_range:Range<u64>}");
        success &= assert_type_fprint::<AllocatorValueV32>(
            "enum {None,Abs(count:u64,owner_object_id:u64)}",
        );
        success &= assert_type_fprint::<EncryptedMutationsV32>("struct {transactions:Vec<(struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},u64,)>,data:Vec<u8>,mutations_key_roll:Vec<(usize,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>}");
        success &= assert_type_fprint::<JournalRecordV37>("enum {EndBlock,Mutation(object_id:u64,mutation:enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u8>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}}),CreateInternalDir(u64)}),Commit,Discard(u64),DidFlushDevice(u64),DataChecksums(Range<u64>,enum {None,Fletcher(Vec<u8>)})}");
        success &= assert_type_fprint::<LayerInfoV32>(
            "struct {key_value_version:struct {major:u32,minor:u8},block_size:u64}",
        );
        success &= assert_type_fprint::<FsverityMetadataV33>(
            "struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>}",
        );
        success &= assert_type_fprint::<MutationV37>("enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u8>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}}),CreateInternalDir(u64)}");
        success &= assert_type_fprint::<ObjectKeyV32>("struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}}");
        success &= assert_type_fprint::<ObjectValueV37>("enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u8>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})}");
        success &= assert_type_fprint::<StoreInfoV36>("struct {guid:[u8;16],last_object_id:u64,layers:Vec<u64>,root_directory_object_id:u64,graveyard_directory_object_id:u64,object_count:u64,mutations_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>,mutations_cipher_offset:u64,encrypted_mutations_object_id:u64,object_id_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>,internal_directory_object_id:u64}");
        success &= assert_type_fprint::<SuperBlockHeaderV32>("struct {guid:<[u8;16]>,generation:u64,root_parent_store_object_id:u64,root_parent_graveyard_directory_object_id:u64,root_store_object_id:u64,allocator_object_id:u64,journal_object_id:u64,journal_checkpoint:struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},super_block_journal_file_offset:u64,journal_file_offsets:HashMap<u64,u64>,borrowed_metadata_space:u64,earliest_version:struct {major:u32,minor:u8}}");
        success &= assert_type_fprint::<SuperBlockRecordV37>("enum {Extent(Range<u64>),ObjectItem(struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u8>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64}),End}");
        assert!(success, "One or more versioned types have different type fingerprint.");
    }

    #[test]
    fn type_fprint_v36() {
        let mut success = true;
        success &= assert_type_fprint::<AllocatorInfoV32>("struct {layers:Vec<u64>,allocated_bytes:BTreeMap<u64,u64>,marked_for_deletion:HashSet<u64>,limit_bytes:BTreeMap<u64,u64>}");
        success &= assert_type_fprint::<AllocatorKeyV32>("struct {device_range:Range<u64>}");
        success &= assert_type_fprint::<AllocatorValueV32>(
            "enum {None,Abs(count:u64,owner_object_id:u64)}",
        );
        success &= assert_type_fprint::<EncryptedMutationsV32>("struct {transactions:Vec<(struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},u64,)>,data:Vec<u8>,mutations_key_roll:Vec<(usize,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>}");
        success &= assert_type_fprint::<JournalRecordV36>("enum {EndBlock,Mutation(object_id:u64,mutation:enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}}),CreateInternalDir(u64)}),Commit,Discard(u64),DidFlushDevice(u64),DataChecksums(Range<u64>,Vec<u64>)}");
        success &= assert_type_fprint::<LayerInfoV32>(
            "struct {key_value_version:struct {major:u32,minor:u8},block_size:u64}",
        );
        success &= assert_type_fprint::<FsverityMetadataV33>(
            "struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>}",
        );
        success &= assert_type_fprint::<MutationV33>("enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}}),CreateInternalDir(u64)}");
        success &= assert_type_fprint::<ObjectKeyV32>("struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}}");
        success &= assert_type_fprint::<ObjectValueV33>("enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})}");
        success &= assert_type_fprint::<StoreInfoV32>("struct {guid:[u8;16],last_object_id:u64,layers:Vec<u64>,root_directory_object_id:u64,graveyard_directory_object_id:u64,object_count:u64,mutations_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>,mutations_cipher_offset:u64,encrypted_mutations_object_id:u64,object_id_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>}");
        success &= assert_type_fprint::<SuperBlockHeaderV32>("struct {guid:<[u8;16]>,generation:u64,root_parent_store_object_id:u64,root_parent_graveyard_directory_object_id:u64,root_store_object_id:u64,allocator_object_id:u64,journal_object_id:u64,journal_checkpoint:struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},super_block_journal_file_offset:u64,journal_file_offsets:HashMap<u64,u64>,borrowed_metadata_space:u64,earliest_version:struct {major:u32,minor:u8}}");
        success &= assert_type_fprint::<SuperBlockRecordV33>("enum {Extent(Range<u64>),ObjectItem(struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64}),End}");
        assert!(success, "One or more versioned types have different type fingerprint.");
    }

    #[test]
    fn type_fprint_v35() {
        // NOTE: Versions 34 and 35 are identical, so type_fprint_v34() is not necessary.
        let mut success = true;
        success &= assert_type_fprint::<AllocatorInfoV32>("struct {layers:Vec<u64>,allocated_bytes:BTreeMap<u64,u64>,marked_for_deletion:HashSet<u64>,limit_bytes:BTreeMap<u64,u64>}");
        success &= assert_type_fprint::<AllocatorKeyV32>("struct {device_range:Range<u64>}");
        success &= assert_type_fprint::<AllocatorValueV32>(
            "enum {None,Abs(count:u64,owner_object_id:u64)}",
        );
        success &= assert_type_fprint::<EncryptedMutationsV32>("struct {transactions:Vec<(struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},u64,)>,data:Vec<u8>,mutations_key_roll:Vec<(usize,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>}");
        success &= assert_type_fprint::<JournalRecordV34>("enum {EndBlock,Mutation(object_id:u64,mutation:enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}}),CreateInternalDir(u64)}),Commit,Discard(u64),DidFlushDevice(u64),DataChecksums(Range<u64>,Vec<u64>)}");
        success &= assert_type_fprint::<LayerInfoV32>(
            "struct {key_value_version:struct {major:u32,minor:u8},block_size:u64}",
        );
        success &= assert_type_fprint::<FsverityMetadataV33>(
            "struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>}",
        );
        success &= assert_type_fprint::<MutationV33>("enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}}),CreateInternalDir(u64)}");
        success &= assert_type_fprint::<ObjectKeyV32>("struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}}");
        success &= assert_type_fprint::<ObjectValueV33>("enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})}");
        success &= assert_type_fprint::<StoreInfoV32>("struct {guid:[u8;16],last_object_id:u64,layers:Vec<u64>,root_directory_object_id:u64,graveyard_directory_object_id:u64,object_count:u64,mutations_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>,mutations_cipher_offset:u64,encrypted_mutations_object_id:u64,object_id_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>}");
        success &= assert_type_fprint::<SuperBlockHeaderV32>("struct {guid:<[u8;16]>,generation:u64,root_parent_store_object_id:u64,root_parent_graveyard_directory_object_id:u64,root_store_object_id:u64,allocator_object_id:u64,journal_object_id:u64,journal_checkpoint:struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},super_block_journal_file_offset:u64,journal_file_offsets:HashMap<u64,u64>,borrowed_metadata_space:u64,earliest_version:struct {major:u32,minor:u8}}");
        success &= assert_type_fprint::<SuperBlockRecordV33>("enum {Extent(Range<u64>),ObjectItem(struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64}),End}");
        assert!(success, "One or more versioned types have different type fingerprint.");
    }

    #[test]
    fn type_fprint_v33() {
        let mut success = true;
        success &= assert_type_fprint::<AllocatorInfoV32>("struct {layers:Vec<u64>,allocated_bytes:BTreeMap<u64,u64>,marked_for_deletion:HashSet<u64>,limit_bytes:BTreeMap<u64,u64>}");
        success &= assert_type_fprint::<AllocatorKeyV32>("struct {device_range:Range<u64>}");
        success &= assert_type_fprint::<AllocatorValueV32>(
            "enum {None,Abs(count:u64,owner_object_id:u64)}",
        );
        success &= assert_type_fprint::<EncryptedMutationsV32>("struct {transactions:Vec<(struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},u64,)>,data:Vec<u8>,mutations_key_roll:Vec<(usize,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>}");
        success &= assert_type_fprint::<JournalRecordV33>("enum {EndBlock,Mutation(object_id:u64,mutation:enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}}),CreateInternalDir(u64)}),Commit,Discard(u64),DidFlushDevice(u64)}");
        success &= assert_type_fprint::<LayerInfoV32>(
            "struct {key_value_version:struct {major:u32,minor:u8},block_size:u64}",
        );
        success &= assert_type_fprint::<FsverityMetadataV33>(
            "struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>}",
        );
        success &= assert_type_fprint::<MutationV33>("enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}}),CreateInternalDir(u64)}");
        success &= assert_type_fprint::<ObjectKeyV32>("struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}}");
        success &= assert_type_fprint::<ObjectValueV33>("enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})}");
        success &= assert_type_fprint::<StoreInfoV32>("struct {guid:[u8;16],last_object_id:u64,layers:Vec<u64>,root_directory_object_id:u64,graveyard_directory_object_id:u64,object_count:u64,mutations_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>,mutations_cipher_offset:u64,encrypted_mutations_object_id:u64,object_id_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>}");
        success &= assert_type_fprint::<SuperBlockHeaderV32>("struct {guid:<[u8;16]>,generation:u64,root_parent_store_object_id:u64,root_parent_graveyard_directory_object_id:u64,root_store_object_id:u64,allocator_object_id:u64,journal_object_id:u64,journal_checkpoint:struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},super_block_journal_file_offset:u64,journal_file_offsets:HashMap<u64,u64>,borrowed_metadata_space:u64,earliest_version:struct {major:u32,minor:u8}}");
        success &= assert_type_fprint::<SuperBlockRecordV33>("enum {Extent(Range<u64>),ObjectItem(struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)}),VerifiedAttribute(size:u64,fsverity_metadata:struct {root_digest:enum {Sha256([u8;32]),Sha512(Vec<u8>)},salt:Vec<u8>})},sequence:u64}),End}");
        assert!(success, "One or more versioned types have different type fingerprint.");
    }

    #[test]
    fn type_fprint_v32() {
        let mut success = true;
        success &= assert_type_fprint::<AllocatorInfoV32>("struct {layers:Vec<u64>,allocated_bytes:BTreeMap<u64,u64>,marked_for_deletion:HashSet<u64>,limit_bytes:BTreeMap<u64,u64>}");
        success &= assert_type_fprint::<AllocatorKeyV32>("struct {device_range:Range<u64>}");
        success &= assert_type_fprint::<AllocatorValueV32>(
            "enum {None,Abs(count:u64,owner_object_id:u64)}",
        );
        success &= assert_type_fprint::<EncryptedMutationsV32>("struct {transactions:Vec<(struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},u64,)>,data:Vec<u8>,mutations_key_roll:Vec<(usize,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>}");
        success &= assert_type_fprint::<JournalRecordV32>("enum {EndBlock,Mutation(object_id:u64,mutation:enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)})},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}})}),Commit,Discard(u64),DidFlushDevice(u64)}");
        success &= assert_type_fprint::<LayerInfoV32>(
            "struct {key_value_version:struct {major:u32,minor:u8},block_size:u64}",
        );
        success &= assert_type_fprint::<MutationV32>("enum {ObjectStore(struct {item:struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)})},sequence:u64},op:enum {Insert,ReplaceOrInsert,Merge}}),EncryptedObjectStore(Box<[u8]>),Allocator(enum {Allocate(device_range:struct {Range<u64>},owner_object_id:u64),Deallocate(device_range:struct {Range<u64>},owner_object_id:u64),SetLimit(owner_object_id:u64,bytes:u64),MarkForDeletion(u64)}),BeginFlush,EndFlush,DeleteVolume,UpdateBorrowed(u64),UpdateMutationsKey(struct {struct {wrapping_key_id:u64,key:WrappedKeyBytes}})}");
        success &= assert_type_fprint::<ObjectKeyV32>("struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}}");
        success &= assert_type_fprint::<ObjectValueV32>("enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)})}");
        success &= assert_type_fprint::<StoreInfoV32>("struct {guid:[u8;16],last_object_id:u64,layers:Vec<u64>,root_directory_object_id:u64,graveyard_directory_object_id:u64,object_count:u64,mutations_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>,mutations_cipher_offset:u64,encrypted_mutations_object_id:u64,object_id_key:Option<struct {wrapping_key_id:u64,key:WrappedKeyBytes}>}");
        success &= assert_type_fprint::<SuperBlockHeaderV32>("struct {guid:<[u8;16]>,generation:u64,root_parent_store_object_id:u64,root_parent_graveyard_directory_object_id:u64,root_store_object_id:u64,allocator_object_id:u64,journal_object_id:u64,journal_checkpoint:struct {file_offset:u64,checksum:u64,version:struct {major:u32,minor:u8}},super_block_journal_file_offset:u64,journal_file_offsets:HashMap<u64,u64>,borrowed_metadata_space:u64,earliest_version:struct {major:u32,minor:u8}}");
        success &= assert_type_fprint::<SuperBlockRecordV32>("enum {Extent(Range<u64>),ObjectItem(struct {key:struct {object_id:u64,data:enum {Object,Keys,Attribute(u64,enum {Attribute,Extent(struct {range:Range<u64>})}),Child(name:String),GraveyardEntry(object_id:u64),Project(project_id:u64,property:enum {Limit,Usage}),ExtendedAttribute(name:Vec<u8>),GraveyardAttributeEntry(object_id:u64,attribute_id:u64)}},value:enum {None,Some,Object(kind:enum {File(refs:u64),Directory(sub_dirs:u64),Graveyard,Symlink(refs:u64,link:Vec<u8>)},attributes:struct {creation_time:struct {secs:u64,nanos:u32},modification_time:struct {secs:u64,nanos:u32},project_id:u64,posix_attributes:Option<struct {mode:u32,uid:u32,gid:u32,rdev:u64}>,allocated_size:u64,access_time:struct {secs:u64,nanos:u32},change_time:struct {secs:u64,nanos:u32}}),Keys(enum {AES256XTS(struct {Vec<(u64,struct {wrapping_key_id:u64,key:WrappedKeyBytes},)>})}),Attribute(size:u64),Extent(enum {None,Some(device_offset:u64,checksums:enum {None,Fletcher(Vec<u64>)},key_id:u64)}),Child(struct {object_id:u64,object_descriptor:enum {File,Directory,Volume,Symlink}}),Trim,BytesAndNodes(bytes:i64,nodes:i64),ExtendedAttribute(enum {Inline(Vec<u8>),AttributeId(u64)})},sequence:u64}),End}");
        assert!(success, "One or more versioned types have different type fingerprint.");
    }
}
