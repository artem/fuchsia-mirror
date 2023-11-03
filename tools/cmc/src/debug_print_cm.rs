// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error::Error;
use cm_rust::FidlIntoNative as _;
use fidl::unpersist;
use fidl_fuchsia_component_decl as fdecl;
use std::{fs::read, path::PathBuf};

pub fn debug_print_cm(file: &PathBuf) -> Result<(), Error> {
    let bytes = read(file).map_err(Error::Io)?;
    let fidl_repr = unpersist::<fdecl::Component>(&bytes).map_err(Error::FidlEncoding)?;
    let cm_repr = fidl_repr.fidl_into_native();
    println!("{:#?}", cm_repr);
    Ok(())
}
