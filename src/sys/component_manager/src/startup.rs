// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Error},
    cm_types::Url,
};

// Used when component manager is started with the "--boot" flag by userboot.
const BOOT_CONFIG: &str = "/boot/config/component_manager";
const BOOT_ROOT_COMPONENT_URL: &str = "fuchsia-boot:///root#meta/root.cm";

/// Command line arguments that control component_manager's behavior. Use [Arguments::from_args()]
/// or [Arguments::new()] to create an instance.
// structopt would be nice to use here but the binary size impact from clap - which it depends on -
// is too large.
#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub struct Arguments {
    /// URL of the root component to launch.
    pub root_component_url: Option<Url>,

    /// Load component_manager's configuration from this path.
    pub config: String,

    /// Whether to have component manager host bootfs (which backs the '/boot' directory).
    pub host_bootfs: bool,

    /// Whether component manager should apply the default boot arguments. This is passed by
    /// userboot when loading the root component manager to avoid hardcoding component manager
    /// specific logic in userboot.
    pub boot: bool,

    /// Whether the builtin runner will be added to the root environment.
    ///
    /// TODO(fxbug.dev/305862055): Remove this feature gate.
    pub add_builtin_runner: bool,
}

impl Arguments {
    /// Parse `Arguments` from [std::env::args()].
    ///
    /// See [Arguments::new()] for more details.
    pub fn from_args() -> Result<Self, Error> {
        // Ignore first argument with executable name, then delegate to generic iterator impl.
        Self::new(std::env::args().skip(1))
    }

    /// Parse `Arguments` from the given String Iterator.
    ///
    /// This parser is relatively simple since component_manager is not a user-facing binary that
    /// requires or would benefit from more flexible UX. Recognized arguments are extracted from
    /// the given Iterator and used to create the returned struct. Unrecognized flags starting with
    /// "--" result in an error being returned. A single non-flag argument is expected for the root
    /// component URL. However, this field may be specified in the config file instead.
    fn new<I>(iter: I) -> Result<Self, Error>
    where
        I: IntoIterator<Item = String>,
    {
        let mut iter = iter.into_iter();
        let mut args = Self::default();
        while let Some(arg) = iter.next() {
            if arg == "--config" {
                args.config = match iter.next() {
                    Some(config) => config,
                    None => return Err(format_err!("No value given for '--config'")),
                }
            } else if arg == "--host_bootfs" {
                args.host_bootfs = true;
            } else if arg == "--boot" {
                args = Arguments {
                    root_component_url: Some(
                        Url::new(BOOT_ROOT_COMPONENT_URL.to_string()).unwrap(),
                    ),
                    config: BOOT_CONFIG.to_string(),
                    host_bootfs: true,
                    boot: true,
                    add_builtin_runner: false,
                };
            } else if arg == "--add_builtin_runner" {
                args.add_builtin_runner = true;
            } else if arg.starts_with("--") {
                return Err(format_err!("Unrecognized flag: {}", arg));
            } else {
                if args.root_component_url.is_some() {
                    return Err(format_err!("Multiple non-flag arguments given"));
                }
                match Url::new(arg) {
                    Ok(url) => args.root_component_url = Some(url),
                    Err(err) => {
                        return Err(format_err!("Failed to parse root_component_url: {:?}", err));
                    }
                }
            }
        }

        if args.config.is_empty() {
            return Err(format_err!("No config file path found"));
        }

        Ok(args)
    }

    /// Returns a usage message for the supported arguments.
    pub fn usage() -> String {
        format!(
            "Usage: {} [options] --config <path-to-config> <root-component-url>\n\
             Options:\n\
             --use-builtin-process-launcher   Provide and use a built-in implementation of\n\
             fuchsia.process.Launcher\n
             --maintain-utc-clock             Create and vend a UTC kernel clock through a\n\
             built-in implementation of fuchsia.time.Maintenance.\n\
             Should only be used with the root component_manager.\n",
            std::env::args().next().unwrap_or("component_manager".to_string())
        )
    }
}

#[cfg(test)]
mod tests {
    use {super::*, lazy_static::lazy_static};

    lazy_static! {
        static ref CONFIG_FILENAME: fn() -> String = || String::from("foo");
        static ref CONFIG_FLAG: fn() -> String = || String::from("--config");
        static ref BOOT_FLAG: fn() -> String = || String::from("--boot");
        static ref ADD_BUILTIN_RUNNER_FLAG: fn() -> String =
            || String::from("--add_builtin_runner");
        static ref DUMMY_URL: fn() -> Url =
            || Url::new("fuchsia-pkg://fuchsia.com/pkg#meta/component.cm".to_owned()).unwrap();
        static ref DUMMY_URL_AS_STR: fn() -> String = || DUMMY_URL().as_str().to_owned();
    }

    #[fuchsia::test]
    fn no_arguments() {
        assert!(Arguments::new(vec![]).is_err());
    }

    #[fuchsia::test]
    fn no_config_file() {
        assert!(Arguments::new(vec![DUMMY_URL_AS_STR(),]).is_err());
        assert!(Arguments::new(vec![CONFIG_FLAG(),]).is_err());
    }

    #[fuchsia::test]
    fn multiple_component_urls() {
        assert!(Arguments::new(vec![DUMMY_URL_AS_STR(), DUMMY_URL_AS_STR(),]).is_err());
    }

    #[fuchsia::test]
    fn bad_flag() {
        let unknown_flag = || String::from("--unknown");

        assert!(Arguments::new(vec![unknown_flag()]).is_err());
        assert!(Arguments::new(vec![unknown_flag(), DUMMY_URL_AS_STR()]).is_err());
        assert!(Arguments::new(vec![DUMMY_URL_AS_STR(), unknown_flag()]).is_err());
    }

    #[fuchsia::test]
    fn bad_component_url() {
        let bad_url = || String::from("not a valid url");

        assert!(Arguments::new(vec![CONFIG_FLAG(), CONFIG_FILENAME(), bad_url(),]).is_err());
        assert!(Arguments::new(vec![bad_url(), CONFIG_FLAG(), CONFIG_FILENAME(),]).is_err());
    }

    #[fuchsia::test]
    fn parse_arguments() {
        let expected_arguments = Arguments {
            config: CONFIG_FILENAME(),
            root_component_url: Some(DUMMY_URL()),
            ..Default::default()
        };

        // Single positional argument with no options is parsed correctly.
        assert_eq!(
            Arguments::new(vec![CONFIG_FLAG(), CONFIG_FILENAME(), DUMMY_URL_AS_STR()])
                .expect("Unexpected error with just URL"),
            expected_arguments
        );

        // Options are parsed correctly and do not depend on order.
        assert_eq!(
            Arguments::new(vec![DUMMY_URL_AS_STR(), CONFIG_FLAG(), CONFIG_FILENAME()])
                .expect("Unexpected error with option"),
            expected_arguments
        );

        // Parses argument with root component url omitted.
        assert_eq!(
            Arguments::new(vec![CONFIG_FLAG(), CONFIG_FILENAME()])
                .expect("Unexpected error with no URL"),
            Arguments { config: CONFIG_FILENAME(), ..Default::default() }
        );
    }

    #[fuchsia::test]
    fn parse_add_builtin_runner() {
        let expected_arguments =
            Arguments { config: CONFIG_FILENAME(), add_builtin_runner: true, ..Default::default() };

        assert_eq!(
            Arguments::new(vec![CONFIG_FLAG(), CONFIG_FILENAME(), ADD_BUILTIN_RUNNER_FLAG()])
                .unwrap(),
            expected_arguments
        );
    }

    #[fuchsia::test]
    fn boot_argument_sets_defaults() {
        let expected_arguments = Arguments {
            root_component_url: Some(Url::new(BOOT_ROOT_COMPONENT_URL.to_string()).unwrap()),
            config: BOOT_CONFIG.to_string(),
            host_bootfs: true,
            boot: true,
            add_builtin_runner: false,
        };

        assert_eq!(
            Arguments::new(vec![BOOT_FLAG()]).expect("Failed to parse arguments"),
            expected_arguments
        );
    }
}
