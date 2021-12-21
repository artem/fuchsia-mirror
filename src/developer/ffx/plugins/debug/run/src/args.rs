// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {argh::FromArgs, ffx_core::ffx_command};

/// Options for "ffx debug run".
#[ffx_command()]
#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand, name = "run", description = "start the debugger and connect to the target")]
pub struct RunCommand {
    /// extra arguments passed to zxdb. Any arguments starting with "-" must be after a "--" separator.
    #[argh(positional)]
    pub zxdb_args: Vec<String>,
}
