// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This is an implementation of "simple" pseudo directories.
//! Use [`mod@crate::directory::immutable::simple`]
//! to construct actual instances.  See [`Simple`] for details.

use crate::{
    common::{rights_to_posix_mode_bits, send_on_open_with_error},
    directory::{
        connection::DerivedConnection,
        dirents_sink,
        entry::{DirectoryEntry, EntryInfo},
        entry_container::{Directory, DirectoryWatcher},
        helper::{AlreadyExists, DirectlyMutable, NotDirectory},
        immutable::connection::ImmutableConnection,
        mutable::{connection::MutableConnection, entry_constructor::NewEntryType},
        traversal_position::TraversalPosition,
        watchers::{
            event_producers::{SingleNameEventProducer, StaticVecEventProducer},
            Watchers,
        },
    },
    execution_scope::ExecutionScope,
    name::Name,
    node::Node,
    path::Path,
    protocols::ProtocolsExt,
    ObjectRequestRef, ToObjectRequest,
};

use {
    async_trait::async_trait,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio,
    fuchsia_zircon_status::Status,
    std::{
        boxed::Box,
        clone::Clone,
        collections::{
            btree_map::{self, Entry},
            BTreeMap,
        },
        iter,
        marker::PhantomData,
        ops::DerefMut,
        sync::{Arc, Mutex},
    },
};

/// An implementation of a "simple" pseudo directory.  This directory holds a set of entries,
/// allowing the server to add or remove entries via the
/// [`crate::directory::helper::DirectlyMutable::add_entry()`] and
/// [`crate::directory::helper::DirectlyMutable::remove_entry`] methods, and, depending on the
/// connection been used (see [`ImmutableConnection`] or [`MutableConnection`])
/// it may also allow the clients to modify the entries as well.  This is a common implementation
/// for [`mod@crate::directory::immutable::simple`] and [`mod@crate::directory::mutable::simple`].
pub struct Simple<Connection> {
    inner: Mutex<Inner>,

    // The inode for this directory. This should either be unique within this VFS, or INO_UNKNOWN.
    inode: u64,

    _connection: PhantomData<Connection>,

    not_found_handler: Mutex<Option<Box<dyn FnMut(&str) + Send + Sync + 'static>>>,
}

struct Inner {
    entries: BTreeMap<Name, Arc<dyn DirectoryEntry>>,

    watchers: Watchers,
}

impl<Connection> Simple<Connection>
where
    Connection: DerivedConnection + 'static,
{
    pub(super) fn new(inode: u64) -> Arc<Self> {
        Arc::new(Simple {
            inner: Mutex::new(Inner { entries: BTreeMap::new(), watchers: Watchers::new() }),
            _connection: PhantomData,
            inode,
            not_found_handler: Mutex::new(None),
        })
    }

    fn get_or_insert_entry(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        name: &str,
        path: &Path,
    ) -> Result<Arc<dyn DirectoryEntry>, Status> {
        let mut this = self.inner.lock().unwrap();

        match this.entries.get(name) {
            Some(entry) => {
                if flags.intersects(fio::OpenFlags::CREATE_IF_ABSENT) {
                    return Err(Status::ALREADY_EXISTS);
                }

                Ok(entry.clone())
            }
            None => {
                if !flags.intersects(fio::OpenFlags::CREATE | fio::OpenFlags::CREATE_IF_ABSENT) {
                    return Err(Status::NOT_FOUND);
                }
                let entry_type = NewEntryType::from_flags(flags, path.is_dir())?;
                let entry = create_entry(
                    Connection::MUTABLE,
                    scope.clone(),
                    self.clone(),
                    entry_type,
                    name,
                    path,
                )?;

                let name: Name = name.to_string().try_into()?;
                let _ = this.entries.insert(name, entry.clone());
                Ok(entry)
            }
        }
    }

    // Attempts to find or create a directory entry given the specified protocols.
    fn get_or_insert_entry_from_protocols(
        self: Arc<Self>,
        scope: ExecutionScope,
        protocols: &fio::ConnectionProtocols,
        name: &str,
        path: &Path,
    ) -> Result<Arc<dyn DirectoryEntry>, Status> {
        let mut this = self.inner.lock().unwrap();

        match this.entries.get(name) {
            Some(entry) => {
                if protocols.open_mode() == fio::OpenMode::AlwaysCreate {
                    return Err(Status::ALREADY_EXISTS);
                }
                Ok(entry.clone())
            }
            None => {
                let entry = if let fio::ConnectionProtocols::Node(fio::NodeOptions {
                    protocols: Some(node_protocols),
                    ..
                }) = protocols
                {
                    if protocols.open_mode() == fio::OpenMode::OpenExisting {
                        return Err(Status::NOT_FOUND);
                    }
                    let entry_type = NewEntryType::from_protocols(node_protocols)?;
                    create_entry(
                        Connection::MUTABLE,
                        scope.clone(),
                        self.clone(),
                        entry_type,
                        name,
                        path,
                    )?
                } else {
                    return Err(Status::INVALID_ARGS);
                };

                let name: Name = name.to_string().try_into()?;
                let _ = this.entries.insert(name, entry.clone());
                Ok(entry)
            }
        }
    }

    /// The provided function will be called whenever this VFS receives an open request for a path
    /// that is not present in the VFS. The function is invoked with the full path of the missing
    /// entry, relative to the root of this VFS. Typically this function is used for logging.
    pub fn set_not_found_handler(
        self: Arc<Self>,
        handler: Box<dyn FnMut(&str) + Send + Sync + 'static>,
    ) {
        let mut this = self.not_found_handler.lock().unwrap();
        this.replace(handler);
    }

    /// Returns the entry identified by `name`.
    pub fn get_entry(&self, name: &str) -> Result<Arc<dyn DirectoryEntry>, Status> {
        crate::name::validate_name(name)?;

        let this = self.inner.lock().unwrap();
        match this.entries.get(name) {
            Some(entry) => Ok(entry.clone()),
            None => Err(Status::NOT_FOUND),
        }
    }

    /// Gets or inserts an entry (as supplied by the callback `f`).
    pub fn get_or_insert<T: DirectoryEntry>(
        &self,
        name: Name,
        f: impl FnOnce() -> Arc<T>,
    ) -> Arc<dyn DirectoryEntry> {
        self.inner
            .lock()
            .unwrap()
            .entries
            .entry(name)
            .or_insert_with(|| f() as Arc<dyn DirectoryEntry>)
            .clone()
    }

    /// Filters and maps all directory entries.  It is similar to std::iter::Iterator::filter_map
    /// except that it always return a Vec rather than an iterator.
    pub fn filter_map<B>(&self, f: impl Fn(&str, &Arc<dyn DirectoryEntry>) -> Option<B>) -> Vec<B> {
        self.inner.lock().unwrap().entries.iter().filter_map(|(k, v)| f(k, v)).collect()
    }

    /// Returns true if any entry matches the given predicate.
    pub fn any(&self, f: impl Fn(&str, &Arc<dyn DirectoryEntry>) -> bool) -> bool {
        self.inner.lock().unwrap().entries.iter().any(|(k, v)| f(k, v))
    }
}

impl<Connection> DirectoryEntry for Simple<Connection>
where
    Connection: DerivedConnection + 'static,
{
    fn open(
        self: Arc<Self>,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        mut path: Path,
        server_end: ServerEnd<fio::NodeMarker>,
    ) {
        // See if the path has a next segment, if so we want to traverse down
        // the directory. Otherwise we've arrived at the right directory.
        let (name, path_ref) = match path.next_with_ref() {
            (path_ref, Some(name)) => (name, path_ref),
            (_, None) => {
                flags.to_object_request(server_end).handle(|object_request| {
                    if Connection::MUTABLE {
                        object_request.spawn_connection(
                            scope,
                            self,
                            flags,
                            MutableConnection::create,
                        )
                    } else {
                        object_request.spawn_connection(
                            scope,
                            self,
                            flags,
                            ImmutableConnection::create,
                        )
                    }
                });
                return;
            }
        };

        // Create a copy so if this fails to open we can call the not found handler
        let ref_copy = self.clone();

        // Do not hold the mutex more than necessary and the Mutex is not re-entrant.  So we need to
        // make sure to release the lock before we call `open()` is it may turn out to be a
        // recursive call, in case the directory contains itself directly or through a number of
        // other directories.  `get_entry` is responsible for locking `self` and it will unlock it
        // before returning.

        let res = if !path_ref.is_empty() {
            self.get_entry(name)
        } else {
            self.get_or_insert_entry(scope.clone(), flags, name, path_ref)
        };
        let describe = flags.intersects(fio::OpenFlags::DESCRIBE);
        match res {
            Err(status) => {
                send_on_open_with_error(describe, server_end, status);

                let mut handler = ref_copy.not_found_handler.lock().unwrap();
                if let Some(handler) = handler.as_mut() {
                    handler(path_ref.as_str());
                }
            }
            Ok(entry) => {
                entry.open(scope, flags, path, server_end);
            }
        }
    }

    fn open2(
        self: Arc<Self>,
        scope: ExecutionScope,
        mut path: Path,
        protocols: fio::ConnectionProtocols,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<(), Status> {
        // See if the path has a next segment, if so we want to traverse down the directory.
        // Otherwise we've arrived at the right directory.
        let (name, path_ref) = match path.next_with_ref() {
            (path_ref, Some(name)) => (name, path_ref),
            (_, None) => {
                object_request.take().handle(|object_request| {
                    if Connection::MUTABLE {
                        object_request.spawn_connection(
                            scope,
                            self,
                            protocols,
                            MutableConnection::create,
                        )
                    } else {
                        object_request.spawn_connection(
                            scope,
                            self,
                            protocols,
                            ImmutableConnection::create,
                        )
                    }
                });
                return Ok(());
            }
        };

        // Create a copy so if this fails to open we can call the not found handler
        let ref_copy = self.clone();

        let entry = if path_ref.is_empty() {
            self.get_or_insert_entry_from_protocols(scope.clone(), &protocols, name, path_ref)
        } else {
            self.get_entry(name)
        };

        match entry {
            Ok(entry) => entry.open2(scope, path, protocols, object_request),
            Err(e) => {
                let mut handler = ref_copy.not_found_handler.lock().unwrap();
                if let Some(handler) = handler.as_mut() {
                    handler(path_ref.as_str());
                }
                Err(e)
            }
        }
    }

    fn entry_info(&self) -> EntryInfo {
        EntryInfo::new(self.inode, fio::DirentType::Directory)
    }
}

#[async_trait]
impl<Connection: DerivedConnection + 'static> Node for Simple<Connection> {
    async fn get_attrs(&self) -> Result<fio::NodeAttributes, Status> {
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_DIRECTORY
                | rights_to_posix_mode_bits(
                    /*r*/ true,
                    /*w*/ Connection::MUTABLE,
                    /*x*/ true,
                ),
            id: self.inode,
            content_size: 0,
            storage_size: 0,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        })
    }

    async fn get_attributes(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, Status> {
        Ok(attributes!(
            requested_attributes,
            Mutable { creation_time: 0, modification_time: 0, mode: 0, uid: 0, gid: 0, rdev: 0 },
            Immutable {
                protocols: fio::NodeProtocolKinds::DIRECTORY,
                abilities: fio::Operations::GET_ATTRIBUTES
                    | fio::Operations::UPDATE_ATTRIBUTES
                    | fio::Operations::ENUMERATE
                    | fio::Operations::TRAVERSE
                    | fio::Operations::MODIFY_DIRECTORY,
                content_size: 0,
                storage_size: 0,
                link_count: 1,
                id: self.inode,
            }
        ))
    }
}

#[async_trait]
impl<Connection> Directory for Simple<Connection>
where
    Connection: DerivedConnection + 'static,
{
    async fn read_dirents<'a>(
        &'a self,
        pos: &'a TraversalPosition,
        sink: Box<dyn dirents_sink::Sink>,
    ) -> Result<(TraversalPosition, Box<dyn dirents_sink::Sealed>), Status> {
        use dirents_sink::AppendResult;

        let this = self.inner.lock().unwrap();

        let (mut sink, entries_iter) = match pos {
            TraversalPosition::Start => {
                match sink.append(&EntryInfo::new(self.inode, fio::DirentType::Directory), ".") {
                    AppendResult::Ok(sink) => {
                        // I wonder why, but rustc can not infer T in
                        //
                        //   pub fn range<T, R>(&self, range: R) -> Range<K, V>
                        //   where
                        //     K: Borrow<T>,
                        //     R: RangeBounds<T>,
                        //     T: Ord + ?Sized,
                        //
                        // for some reason here.  It says:
                        //
                        //   error[E0283]: type annotations required: cannot resolve `_: std::cmp::Ord`
                        //
                        // pointing to "range".  Same for two the other "range()" invocations
                        // below.
                        (sink, this.entries.range::<Name, _>(..))
                    }
                    AppendResult::Sealed(sealed) => {
                        let new_pos = match this.entries.keys().next() {
                            None => TraversalPosition::End,
                            Some(first_name) => TraversalPosition::Name(first_name.clone().into()),
                        };
                        return Ok((new_pos, sealed));
                    }
                }
            }

            TraversalPosition::Name(next_name) => {
                // The only way to get a `TraversalPosition::Name` is if we returned it in the
                // `AppendResult::Sealed` code path above. Therefore, the conversion from
                // `next_name` to `Name` will never fail in practice.
                let next: Name = next_name.to_owned().try_into().unwrap();
                (sink, this.entries.range::<Name, _>(next..))
            }

            TraversalPosition::Index(_) => unreachable!(),

            TraversalPosition::End => return Ok((TraversalPosition::End, sink.seal())),
        };

        for (name, entry) in entries_iter {
            match sink.append(&entry.entry_info(), &name) {
                AppendResult::Ok(new_sink) => sink = new_sink,
                AppendResult::Sealed(sealed) => {
                    return Ok((TraversalPosition::Name(name.clone().into()), sealed));
                }
            }
        }

        Ok((TraversalPosition::End, sink.seal()))
    }

    fn register_watcher(
        self: Arc<Self>,
        scope: ExecutionScope,
        mask: fio::WatchMask,
        watcher: DirectoryWatcher,
    ) -> Result<(), Status> {
        let mut this = self.inner.lock().unwrap();

        let mut names = StaticVecEventProducer::existing({
            let entry_names = this.entries.keys();
            iter::once(".".to_string()).chain(entry_names.map(|x| x.to_owned().into())).collect()
        });

        let controller = this.watchers.add(scope, self.clone(), mask, watcher);
        controller.send_event(&mut names);
        controller.send_event(&mut SingleNameEventProducer::idle());

        Ok(())
    }

    fn unregister_watcher(self: Arc<Self>, key: usize) {
        let mut this = self.inner.lock().unwrap();
        this.watchers.remove(key);
    }
}

impl<Connection> DirectlyMutable for Simple<Connection>
where
    Connection: DerivedConnection + 'static,
{
    fn add_entry_impl(
        &self,
        name: Name,
        entry: Arc<dyn DirectoryEntry>,
        overwrite: bool,
    ) -> Result<(), AlreadyExists> {
        let mut this = self.inner.lock().unwrap();

        if !overwrite && this.entries.contains_key(&name) {
            return Err(AlreadyExists);
        }

        this.watchers.send_event(&mut SingleNameEventProducer::added(&name));

        let _ = this.entries.insert(name, entry);
        Ok(())
    }

    fn remove_entry_impl(
        &self,
        name: Name,
        must_be_directory: bool,
    ) -> Result<Option<Arc<dyn DirectoryEntry>>, NotDirectory> {
        let mut this = self.inner.lock().unwrap();

        match this.entries.entry(name) {
            Entry::Vacant(_) => Ok(None),
            Entry::Occupied(occupied) => {
                if must_be_directory
                    && occupied.get().entry_info().type_() != fio::DirentType::Directory
                {
                    Err(NotDirectory)
                } else {
                    let (key, value) = occupied.remove_entry();
                    this.watchers.send_event(&mut SingleNameEventProducer::removed(&key));
                    Ok(Some(value))
                }
            }
        }
    }

    fn rename_from(
        &self,
        src: String,
        to: Box<dyn FnOnce(Arc<dyn DirectoryEntry>) -> Result<(), Status>>,
    ) -> Result<(), Status> {
        let src: Name = src.try_into()?;

        let mut this = self.inner.lock().unwrap();

        let Inner { entries, watchers, .. } = this.deref_mut();

        let map_entry = match entries.entry(src.clone()) {
            btree_map::Entry::Vacant(_) => return Err(Status::NOT_FOUND),
            btree_map::Entry::Occupied(map_entry) => map_entry,
        };

        to(map_entry.get().clone())?;

        watchers.send_event(&mut SingleNameEventProducer::removed(&src));

        let _ = map_entry.remove();
        Ok(())
    }

    fn rename_to(
        &self,
        dst: String,
        from: Box<dyn FnOnce() -> Result<Arc<dyn DirectoryEntry>, Status>>,
    ) -> Result<(), Status> {
        let dst: Name = dst.try_into()?;

        let mut this = self.inner.lock().unwrap();

        let entry = from()?;

        this.watchers.send_event(&mut SingleNameEventProducer::added(&dst));

        let _ = this.entries.insert(dst, entry);
        Ok(())
    }

    fn rename_within(&self, src: String, dst: String) -> Result<(), Status> {
        let src: Name = src.try_into()?;
        let dst: Name = dst.try_into()?;

        let mut this = self.inner.lock().unwrap();

        // If src doesn't exist, don't do the other stuff.
        if !this.entries.contains_key(&src) {
            return Err(Status::NOT_FOUND);
        }
        // I assume we should send these events even when `src == dst`.  In practice, a
        // particular client may not be aware that the names match, but may still rely on the fact
        // that the events occur.
        //
        // Watcher protocol expects to produce messages that list only one type of event.  So
        // we will send two independent event messages, each with one name.
        this.watchers.send_event(&mut SingleNameEventProducer::removed(&src));
        this.watchers.send_event(&mut SingleNameEventProducer::added(&dst));

        // We acquire the lock first, as in case `src != dst`, we want to make sure that the
        // recipients of these events can not see the directory in the state before the update.  I
        // assume that `src == dst` is unlikely case, and for the sake of reduction of code
        // duplication we can lock and then immediately unlock.
        //
        // It also provides sequencing for the watchers, as they will always receive `removed`
        // followed by `added`, and there will be no interleaving event in-between.  I think it is
        // not super important, as the watchers should probably be prepared to deal with all kinds
        // of sequences anyways.
        if src == dst {
            return Ok(());
        }

        let entry = match this.entries.remove(&src) {
            // This is truly surprising since this was checked previously, but
            // we leave this in place instead of doing `unwrap` on the chance
            // the earlier check is carelessly removed.
            None => return Err(Status::NOT_FOUND),
            Some(entry) => entry,
        };

        let _ = this.entries.insert(dst, entry);
        Ok(())
    }
}

fn create_entry(
    mutable: bool,
    scope: ExecutionScope,
    parent: Arc<dyn DirectoryEntry>,
    entry_type: NewEntryType,
    name: &str,
    path: &Path,
) -> Result<Arc<dyn DirectoryEntry>, Status> {
    if mutable {
        let entry_constructor = scope.entry_constructor().ok_or(Status::NOT_SUPPORTED)?;
        entry_constructor.create_entry(parent, entry_type, name, path)
    } else {
        Err(Status::NOT_SUPPORTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{assert_event, test_utils::test_file::TestFile};
    use fidl::endpoints::create_proxy;
    use std::sync::{Arc, Mutex};

    #[test]
    fn add_entry_success() {
        let dir = crate::directory::mutable::simple();
        assert_eq!(
            dir.add_entry("path_without_separators", TestFile::read_only(b"test")),
            Ok(()),
            "add entry with valid filename should succeed"
        );
    }

    #[test]
    fn add_entry_error_name_with_path_separator() {
        let dir = crate::directory::mutable::simple();
        let status = dir
            .add_entry("path/with/separators", TestFile::read_only(b"test"))
            .expect_err("add entry with path separator should fail");
        assert_eq!(status, Status::INVALID_ARGS);
    }

    #[test]
    fn add_entry_error_name_too_long() {
        let dir = crate::directory::mutable::simple();
        let status = dir
            .add_entry("a".repeat(10000), TestFile::read_only(b"test"))
            .expect_err("add entry whose name is too long should fail");
        assert_eq!(status, Status::BAD_PATH);
    }

    #[fuchsia::test]
    async fn not_found_handler() {
        let dir = crate::directory::mutable::simple();
        let path_mutex = Arc::new(Mutex::new(None));
        let path_mutex_clone = path_mutex.clone();
        dir.clone().set_not_found_handler(Box::new(move |path| {
            *path_mutex_clone.lock().unwrap() = Some(path.to_string());
        }));

        let sub_dir = crate::directory::mutable::simple();
        let path_mutex_clone = path_mutex.clone();
        sub_dir.clone().set_not_found_handler(Box::new(move |path| {
            *path_mutex_clone.lock().unwrap() = Some(path.to_string());
        }));
        dir.add_entry("dir", sub_dir).expect("add entry with valid filename should succeed");

        dir.add_entry("file", TestFile::read_only(b"test"))
            .expect("add entry with valid filename should succeed");

        let scope = ExecutionScope::new();

        for (path, expectation) in vec![
            (".", None),
            ("does-not-exist", Some("does-not-exist".to_string())),
            ("file", None),
            ("dir", None),
            ("dir/does-not-exist", Some("dir/does-not-exist".to_string())),
        ] {
            let (proxy, server_end) =
                create_proxy::<fio::NodeMarker>().expect("Failed to create connection endpoints");
            let flags = fio::OpenFlags::NODE_REFERENCE | fio::OpenFlags::DESCRIBE;
            let path = Path::validate_and_split(path).unwrap();
            dir.clone().open(scope.clone(), flags, path, server_end.into_channel().into());
            assert_event!(proxy, fio::NodeEvent::OnOpen_ { .. }, {});

            assert_eq!(expectation, path_mutex.lock().unwrap().take());
        }
    }
}
