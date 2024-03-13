// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{ArgsInfo, FromArgs, SubCommands};
use errors::ffx_error;
use ffx_command::{
    return_user_error, CliArgsInfo, Error, FfxCommandLine, FfxContext, FfxToolInfo, MetricsSession,
    Optionality, Result, ToolRunner, ToolSuite,
};
use ffx_config::{environment::ExecutableKind, EnvironmentContext};
use ffx_daemon_proxy::{DaemonVersionCheck, Injection};
use ffx_lib_args::FfxBuiltIn;
use ffx_lib_sub_command::SubCommand;
use fho_search::ExternalSubToolSuite;
use std::{collections::HashSet, os::unix::process::ExitStatusExt, process::ExitStatus, sync::Arc};

/// The command to be invoked and everything it needs to invoke
struct FfxSubCommand {
    app: FfxCommandLine,
    context: EnvironmentContext,
    cmd: FfxBuiltIn,
}

/// The suite of commands FFX supports.
struct FfxSuite {
    context: EnvironmentContext,
    external_commands: ExternalSubToolSuite,
}

#[async_trait::async_trait(?Send)]
impl ToolSuite for FfxSuite {
    async fn from_env(env: &EnvironmentContext) -> Result<Self> {
        let context = env.clone();

        let external_commands = ExternalSubToolSuite::from_env(env).await?;

        Ok(Self { context, external_commands })
    }

    fn global_command_list() -> &'static [&'static argh::CommandInfo] {
        SubCommand::COMMANDS
    }

    async fn get_args_info(&self) -> Result<ffx_command::CliArgsInfo> {
        // Determine if we're handling a subcommand, or need to collect all the info
        //from all the subcommands.
        let argv = Vec::from_iter(std::env::args());
        let cmdline0 =
            FfxCommandLine::from_args_for_help(&argv).bug_context("cmd line for help")?;
        if cmdline0.subcmd_iter().count() > 1 {
            let args = Vec::from_iter(cmdline0.global.subcommand.iter().map(String::as_str));
            let all_info = SubCommand::get_args_info();
            let mut info: Option<ffx_command::CliArgsInfo> = None;
            for c in args {
                if c.starts_with("-") {
                    continue;
                }
                if info.is_none() {
                    info = all_info
                        .commands
                        .iter()
                        .find(|s| s.name == c)
                        .map(|s| s.command.clone().into());
                } else {
                    info = info
                        .unwrap()
                        .commands
                        .iter()
                        .find(|s| s.name == c)
                        .map(|s| s.command.clone().into());
                }
            }
            let args_info = info.ok_or(ffx_command::bug!("No args info found"))?;
            return Ok(args_info);
        } else {
            // Gather information about all the subcommands, both internal and external.
            let mut seen: HashSet<&str> = HashSet::new();
            let mut info: ffx_command::CliArgsInfo = ffx_command::Ffx::get_args_info().into();
            let internal_info: ffx_command::CliArgsInfo = SubCommand::get_args_info().into();
            let external_info = self.external_commands.get_args_info().await?;

            // filter out duplicate commands
            for sub in &internal_info.commands {
                if !seen.contains(sub.name.as_str()) {
                    seen.insert(&sub.name);
                    info.commands.push(sub.clone());
                }
            }
            for sub in &external_info.commands {
                if !seen.contains(sub.name.as_str()) {
                    seen.insert(&sub.name);
                    info.commands.push(sub.clone());
                }
            }
            return Ok(info);
        }
    }

    async fn command_list(&self) -> Vec<FfxToolInfo> {
        let builtin_commands = SubCommand::COMMANDS.iter().copied().map(FfxToolInfo::from);

        builtin_commands.chain(self.external_commands.command_list().await.into_iter()).collect()
    }

    async fn try_runner_from_name(
        &self,
        ffx_cmd: &FfxCommandLine,
    ) -> Result<Option<Box<dyn ToolRunner + '_>>> {
        let argv: Vec<_> = ffx_cmd.all_iter().map(|s| s.to_string()).collect();
        if ffx_cmd.subcmd_iter().count() > 1 {
            let args = Vec::from_iter(ffx_cmd.global.subcommand.iter().map(String::as_str));
            let all_info = SubCommand::get_args_info();

            let mut info = find_info_from_cmd(&args, &all_info.into());

            // If info is none, then it is an external command (or an unknown command)
            if info.is_none() {
                let external_info = self.external_commands.get_args_info().await?;
                info = find_info_from_cmd(&args, &external_info);
            }

            let args_info = info.ok_or(ffx_command::bug!("No args info found"))?;
            // add fake args to the command line for required parameters
            let cmd_args = match build_required_args(&args_info) {
                Ok(fake_args) => fake_args,
                Err(e) => {
                    eprintln!("{e}");
                    return self.try_from_args(&ffx_cmd).await;
                }
            };

            let mut new_argv: Vec<String> = argv.clone();
            new_argv.extend(cmd_args);

            let schema_cmdline =
                FfxCommandLine::from_args_for_help(&new_argv).bug_context("cmd line for schema")?;
            return self.try_from_args(&schema_cmdline).await;
        }
        return self.try_from_args(&ffx_cmd).await;
    }

    async fn try_from_args(
        &self,
        ffx_cmd: &FfxCommandLine,
    ) -> Result<Option<Box<(dyn ToolRunner + '_)>>> {
        let context = self.context.clone();
        let app = ffx_cmd.clone();
        let args = Vec::from_iter(app.global.subcommand.iter().map(String::as_str));
        match args.first().copied() {
            Some("commands") => {
                let mut output = String::new();
                self.print_command_list(&mut output).await.ok();
                let code = 0;
                Err(Error::Help { command: ffx_cmd.command.clone(), output, code })
            }
            Some(name) if SubCommand::COMMANDS.iter().any(|c| c.name == name) => {
                let cmd = FfxBuiltIn::from_args(&Vec::from_iter(ffx_cmd.cmd_iter()), &args)
                    .map_err(|err| Error::from_early_exit(&ffx_cmd.command, err))?;
                Ok(Some(Box::new(FfxSubCommand { cmd, context, app })))
            }
            _ => self.external_commands.try_from_args(ffx_cmd).await,
        }
    }
}

#[async_trait::async_trait(?Send)]
impl ToolRunner for FfxSubCommand {
    /// Whether the given subcommand forces logging to stdout
    fn forces_stdout_log(&self) -> bool {
        match &self.cmd {
            subcommand @ FfxBuiltIn { subcommand: Some(_) } if is_daemon(subcommand) => true,
            _ => false,
        }
    }

    async fn run(self: Box<Self>, metrics: MetricsSession) -> Result<ExitStatus> {
        if self.app.global.machine.is_some()
            && !ffx_lib_suite::ffx_plugin_is_machine_supported(&self.cmd)
        {
            Err(ffx_error!("The machine flag is not supported for this subcommand").into())
        } else if self.app.global.schema && !self.app.global.machine.is_some() {
            Err(ffx_error!("The schema flag requires the machine flag").into())
        } else {
            metrics.print_notice(&mut std::io::stderr()).await?;
            let redacted_args = ffx_lib_suite::ffx_plugin_redact_args(&self.app, &self.cmd);
            let res = run_legacy_subcommand(self.app, self.context, self.cmd)
                .await
                .map(|_| ExitStatus::from_raw(0));
            metrics.command_finished(&res, &redacted_args).await.and(res)
        }
    }
}

/// Builds a vec of arguments that are required for the given info.
/// This is used to create a command line that can be successfully
/// parsed for the info by appending the returned list to the command
/// line that represents the info.
fn build_required_args(info: &CliArgsInfo) -> Result<Vec<String>> {
    let mut args = vec![];
    if !info.commands.is_empty() {
        return_user_error!("Schema for commands with subcommands is not supported");
    }
    for f in &info.flags {
        if f.optionality == Optionality::Required {
            match &f.kind {
                ffx_command::FlagKind::Option { arg_name } => {
                    args.extend_from_slice(&[f.long.clone(), arg_name.clone()])
                }
                ffx_command::FlagKind::Switch => args.extend_from_slice(&[f.long.clone()]),
            };
        }
    }
    for p in &info.positionals {
        if p.optionality == Optionality::Required {
            args.extend_from_slice(&[p.name.clone()])
        }
    }
    Ok(args)
}

/// Finds command line info given the command line args starting with the all_info
/// collection of top level commands.
fn find_info_from_cmd(args: &[&str], all_info: &CliArgsInfo) -> Option<CliArgsInfo> {
    let mut info: Option<CliArgsInfo> = None;

    for c in args {
        // skip options
        if c.starts_with("-") {
            continue;
        }
        if info.is_none() {
            info =
                all_info.commands.iter().find(|s| s.name == *c).map(|s| s.command.clone().into());
        } else {
            info = info
                .unwrap()
                .commands
                .iter()
                .find(|s| s.name == *c)
                .map(|s| s.command.clone().into());
        }
    }
    info
}

async fn run_legacy_subcommand(
    app: FfxCommandLine,
    context: EnvironmentContext,
    subcommand: FfxBuiltIn,
) -> Result<()> {
    let daemon_version_string = DaemonVersionCheck::SameBuildId(context.daemon_version_string()?);
    tracing::debug!("initializing overnet");
    let injector =
        Injection::initialize_overnet(context, None, daemon_version_string, app.global.machine)
            .await?;
    tracing::debug!("Overnet initialized, creating injector");
    let injector: Arc<dyn ffx_core::Injector> = Arc::new(injector);
    ffx_lib_suite::ffx_plugin_impl(&injector, subcommand).await
}

fn is_daemon(subcommand: &FfxBuiltIn) -> bool {
    use ffx_daemon_plugin_args::FfxPluginCommand;
    use ffx_daemon_plugin_sub_command::SubCommand::FfxDaemonStart;
    use SubCommand::FfxDaemonPlugin;
    matches!(
        subcommand,
        FfxBuiltIn {
            subcommand: Some(FfxDaemonPlugin(FfxPluginCommand { subcommand: FfxDaemonStart(_) }))
        }
    )
}

#[fuchsia_async::run_singlethreaded]
async fn main() {
    let result = ffx_command::run::<FfxSuite>(ExecutableKind::MainFfx).await;
    ffx_command::exit(result).await
}

#[cfg(test)]
mod test {
    use super::*;
    use ffx_command::{FlagInfo, FlagKind, PositionalInfo, SubCommandInfo};

    #[fuchsia::test]
    async fn test_try_runner_from_name() {
        let env = ffx_config::test_init().await.expect("test env");
        let suite = FfxSuite::from_env(&env.context).await.expect("ffx suite");

        let cmd = FfxCommandLine::new(None, &["ffx", "target", "list"]).expect("ffx cmdline");
        let runner = suite.try_runner_from_name(&cmd).await.expect("runner from name");
        assert!(runner.is_some());

        let cmd = FfxCommandLine::new(None, &["ffx", "no-known-cmd"]).expect("ffx cmdline");
        let runner = suite.try_runner_from_name(&cmd).await.expect("runner from name");
        assert!(runner.is_none());
    }

    #[fuchsia::test]
    async fn test_build_required_args() {
        let info = CliArgsInfo::default();
        let args = build_required_args(&info).expect("build args");
        let expected: Vec<String> = vec![];
        assert_eq!(args, expected);

        let mut required_info = CliArgsInfo::default();
        required_info.flags.push(FlagInfo {
            kind: FlagKind::Switch,
            optionality: Optionality::Required,
            long: "sw1".to_string(),
            ..Default::default()
        });
        required_info.flags.push(FlagInfo {
            kind: FlagKind::Switch,
            optionality: Optionality::Optional,
            long: "skip".to_string(),
            ..Default::default()
        });
        required_info.flags.push(FlagInfo {
            kind: FlagKind::Option { arg_name: "value".into() },
            optionality: Optionality::Required,
            long: "option".to_string(),
            ..Default::default()
        });
        required_info.positionals.push(PositionalInfo {
            name: "pos1".into(),
            optionality: Optionality::Required,
            ..Default::default()
        });
        let some_args = build_required_args(&required_info).expect("build args");
        let expected: Vec<String> =
            vec!["sw1".into(), "option".into(), "value".into(), "pos1".into()];
        assert_eq!(some_args, expected);
    }

    #[fuchsia::test]
    async fn test_find_info_from_cmd() {
        let args = ["--skip", "target", "list"];
        let all_info = CliArgsInfo::default();
        let actual = find_info_from_cmd(&args, &all_info);
        assert!(actual.is_none());

        let some_info = CliArgsInfo {
            commands: vec![SubCommandInfo {
                name: "target".into(),
                command: CliArgsInfo {
                    name: "target".into(),
                    commands: vec![SubCommandInfo {
                        name: "list".into(),
                        command: CliArgsInfo { name: "list".into(), ..Default::default() },
                    }],
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let actual = find_info_from_cmd(&args, &some_info);
        assert!(actual.is_some());
    }
}
