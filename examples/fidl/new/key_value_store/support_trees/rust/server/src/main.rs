// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context as _, Error},
    // [START diff_1]
    fidl_examples_keyvaluestore_supporttrees::{
        Item, StoreRequest, StoreRequestStream, Value, WriteError,
    },
    // [END diff_1]
    fuchsia_component::server::ServiceFs,
    futures::prelude::*,
    lazy_static::lazy_static,
    regex::Regex,
    std::cell::RefCell,
    std::collections::hash_map::Entry,
    std::collections::HashMap,
    std::str::from_utf8,
};

lazy_static! {
    static ref KEY_VALIDATION_REGEX: Regex =
        Regex::new(r"^[A-Za-z]\w+[A-Za-z0-9]$").expect("Key validation regex failed to compile");
}

// [START diff_2]
// A representation of a key-value store that can contain an arbitrarily deep nesting of other
// key-value stores.
#[allow(dead_code)] // TODO(https://fxbug.dev/318827209)
enum StoreNode {
    Leaf(Option<Vec<u8>>),
    Branch(Box<HashMap<String, StoreNode>>),
}

/// Recursive item writer, which takes a `StoreNode` that may not necessarily be the root node, and
/// writes an entry to it.
fn write_item(
    store: &mut HashMap<String, StoreNode>,
    attempt: Item,
    path: &str,
) -> Result<(), WriteError> {
    // [END diff_2]
    // Validate the key.
    if !KEY_VALIDATION_REGEX.is_match(attempt.key.as_str()) {
        println!("Write error: INVALID_KEY, For key: {}", attempt.key);
        return Err(WriteError::InvalidKey);
    }

    // Write to the store, validating that the key did not already exist.
    match store.entry(attempt.key) {
        Entry::Occupied(entry) => {
            println!("Write error: ALREADY_EXISTS, For key: {}", entry.key());
            Err(WriteError::AlreadyExists)
        }
        Entry::Vacant(entry) => {
            // [START diff_3]
            let key = format!("{}{}", &path, entry.key());
            match attempt.value {
                // Null entries are allowed.
                None => {
                    println!("Wrote value: NONE at key: {}", key);
                    entry.insert(StoreNode::Leaf(None));
                }
                Some(value) => match *value {
                    // If this is a nested store, recursively make a new store to insert at this
                    // position.
                    Value::Store(entry_list) => {
                        // Validate the value - absent stores, items lists with no children, or any
                        // of the elements within that list being empty boxes, are all not allowed.
                        if entry_list.items.is_some() {
                            let items = entry_list.items.unwrap();
                            if !items.is_empty() && items.iter().all(|i| i.is_some()) {
                                let nested_path = format!("{}/", key);
                                let mut nested_store = HashMap::<String, StoreNode>::new();
                                for item in items.into_iter() {
                                    write_item(&mut nested_store, *item.unwrap(), &nested_path)?;
                                }

                                println!("Created branch at key: {}", key);
                                entry.insert(StoreNode::Branch(Box::new(nested_store)));
                                return Ok(());
                            }
                        }

                        println!("Write error: INVALID_VALUE, For key: {}", key);
                        return Err(WriteError::InvalidValue);
                    }

                    // This is a simple leaf node on this branch.
                    Value::Bytes(value) => {
                        // Validate the value.
                        if value.is_empty() {
                            println!("Write error: INVALID_VALUE, For key: {}", key);
                            return Err(WriteError::InvalidValue);
                        }

                        println!("Wrote key: {}, value: {:?}", key, from_utf8(&value).unwrap());
                        entry.insert(StoreNode::Leaf(Some(value)));
                    }
                },
            }
            // [END diff_3]
            Ok(())
        }
    }
}

/// Creates a new instance of the server. Each server has its own bespoke, per-connection instance
/// of the key-value store.
async fn run_server(stream: StoreRequestStream) -> Result<(), Error> {
    // Create a new in-memory key-value store. The store will live for the lifetime of the
    // connection between the server and this particular client.
    // [START diff_4]
    let store = RefCell::new(HashMap::<String, StoreNode>::new());
    // [END diff_4]

    // Serve all requests on the protocol sequentially - a new request is not handled until its
    // predecessor has been processed.
    stream
        .map(|result| result.context("failed request"))
        .try_for_each(|request| async {
            // Match based on the method being invoked.
            match request {
                StoreRequest::WriteItem { attempt, responder } => {
                    println!("WriteItem request received");

                    // The `responder` parameter is a special struct that manages the outgoing reply
                    // to this method call. Calling `send` on the responder exactly once will send
                    // the reply.
                    responder
                        // [START diff_5]
                        .send(write_item(&mut store.borrow_mut(), attempt, ""))
                        // [END diff_5]
                        .context("error sending reply")?;
                    println!("WriteItem response sent");
                }
                StoreRequest::_UnknownMethod { ordinal, .. } => {
                    println!("Received an unknown method with ordinal {ordinal}");
                }
            }
            Ok(())
        })
        .await
}

// A helper enum that allows us to treat a `Store` service instance as a value.
enum IncomingService {
    Store(StoreRequestStream),
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    println!("Started");

    // Add a discoverable instance of our `Store` protocol - this will allow the client to see the
    // server and connect to it.
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(IncomingService::Store);
    fs.take_and_serve_directory_handle()?;
    println!("Listening for incoming connections");

    // The maximum number of concurrent clients that may be served by this process.
    const MAX_CONCURRENT: usize = 10;

    // Serve each connection simultaneously, up to the `MAX_CONCURRENT` limit.
    fs.for_each_concurrent(MAX_CONCURRENT, |IncomingService::Store(stream)| {
        run_server(stream).unwrap_or_else(|e| println!("{:?}", e))
    })
    .await;

    Ok(())
}
