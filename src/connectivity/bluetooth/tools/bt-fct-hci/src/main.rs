// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![recursion_limit = "256"]

mod hci;
mod types;

use {
    anyhow::{Context, Error},
    argh::FromArgs,
    fuchsia_async as fasync,
    futures::StreamExt,
    hci::CommandChannel,
    hex::FromHex,
    types::*,
};

#[derive(Debug, FromArgs)]
/// Command line args
struct Args {
    #[argh(switch, short = 'v')]
    /// enable verbose log output.
    verbose: bool,

    #[argh(subcommand)]
    subcommand: HciSubcommand,
}

#[derive(Debug, FromArgs)]
#[argh(subcommand)]
enum HciSubcommand {
    Raw(RawSubcommand),
    Reset(ResetSubcommand),
    BrEdrScan(BrEdrScanSubcommand),
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "raw")]
/// Send raw HCI command
struct RawSubcommand {
    #[argh(switch, short = 'c')]
    /// continue listening for events.
    continue_listening: bool,

    #[argh(positional)]
    /// hex encoded payload.
    payload: Vec<String>,
}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "reset")]
/// Send HCI reset command
struct ResetSubcommand {}

#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "scan")]
/// Perform an BrEdr scan command
struct BrEdrScanSubcommand {
    #[argh(option, default = "30", short = 't')]
    /// maximum time to scan in seconds (max 62)
    timeout: u8,

    #[argh(option, short = 'f')]
    /// filter address prefix.
    filter: Option<String>,

    #[argh(option, default = "255", short = 'm')]
    /// max number of results (max 255).
    max_results: u8,
}

/// Parses the repeating payload passed in as args to the tool. Attempts to
/// parse all args as hex values. Supports comma, colon, and space seperated
/// bytes and any bytes prefixed with "0x".
fn parse_payload(payload: &[&str]) -> Result<Vec<u8>, String> {
    let payload = payload
        .join(" ")
        .replace(",", " ")
        .replace(":", " ")
        .split(" ")
        .map(|s| s.trim_start_matches("0x"))
        .map(|s| s.trim_start_matches("0X"))
        .collect::<Vec<&str>>()
        .join("");
    Vec::from_hex(payload).map_err(|e| format!("Invalid payload: {}", e))
}

/// Pumps the command channel stream, printing each event packet received.
async fn print_response_loop(
    verbose: bool,
    command_channel: &mut CommandChannel,
    match_opcode: u16,
    continue_listening: bool,
) -> Result<(), Error> {
    // read each packet until we get a response to the opcode we sent
    loop {
        if let Some(packet) = command_channel.next().await {
            let in_opcode = packet.opcode();

            if verbose {
                let pretty_string = packet.decode();
                // pretty print every packet in verbose mode.
                println!("{}", pretty_string);
            } else if in_opcode.is_some() && match_opcode == in_opcode.unwrap() {
                // only print the return packet for the opcode we sent when not
                // verbose.
                println!("{}", packet);
            }

            if !continue_listening && in_opcode.is_some() && match_opcode == in_opcode.unwrap() {
                break;
            }
        } else {
            println!("No response");
            break;
        }
    }
    Ok(())
}

/// Send a raw hex encoded command. Can optionally listen for multiple command packets.
async fn raw_command(
    verbose: bool,
    rawcmd: RawSubcommand,
    mut command_channel: CommandChannel,
) -> Result<(), Error> {
    let ref_vec: Vec<&str> = rawcmd.payload.iter().map(AsRef::as_ref).collect();
    let payload = match parse_payload(&ref_vec[..]) {
        Ok(payload) => payload,
        Err(error) => {
            eprintln!("Error parsing payload: {}", error);
            return Ok(());
        }
    };

    let commands = types::split_commands(&payload)?;

    if commands.len() == 0 {
        eprintln!("No packets passed");
        return Ok(());
    }

    for (n, command) in commands.iter().enumerate() {
        let out_opcode = command.opcode;

        if verbose {
            println!(
                "Sending opcode: {} packet: {}",
                out_opcode,
                hex::encode(&payload[command.range.start..command.range.end])
            );
        }

        command_channel
            .send_command_packet(&payload[command.range.start..command.range.end])
            .context("Error sending HCI packet")?;

        if verbose {
            println!("Awaiting response");
        }

        // only continue listening on the ack of the final command
        let is_last_command = n == commands.len() - 1;
        let continue_listening = rawcmd.continue_listening && is_last_command;
        print_response_loop(verbose, &mut command_channel, out_opcode, continue_listening).await?;
    }

    Ok(())
}

/// Handles a simple HCI command target from the front end. Typically used for simple commands
/// (like vendor test commands) with one command and one event response expected.
async fn basic_command(
    verbose: bool,
    payload: &[u8],
    mut command_channel: CommandChannel,
) -> Result<(), Error> {
    let commands = types::split_commands(&payload)?;

    for command in commands {
        let out_opcode = command.opcode;

        if verbose {
            println!(
                "Sending opcode: {} packet: {}",
                out_opcode,
                hex::encode(&payload[command.range.start..command.range.end])
            );
        }

        command_channel
            .send_command_packet(&payload[command.range.start..command.range.end])
            .context("Error sending HCI packet")?;

        print_response_loop(verbose, &mut command_channel, out_opcode, false).await?;
    }

    Ok(())
}

/// Compares partial filter bt_addr to a full bt_addr.
/// Returns true if there is a partial match.
fn match_filter(addr: &[u8], filter: &[u8]) -> bool {
    assert!(addr.len() == 6);
    assert!(filter.len() <= 6);

    for (a, b) in filter.iter().zip(addr) {
        if a != b {
            return false;
        }
    }

    true
}

/// Processes inquiry result events from the controller.
async fn scan_command(
    verbose: bool,
    filter: Option<String>,
    mut command_channel: CommandChannel,
) -> Result<(), Error> {
    let mac = match filter {
        Some(filter) => match parse_payload(&[&filter]) {
            Ok(filter) => {
                if filter.len() > 6 {
                    eprintln!("invalid mac address: {:?}", filter);
                    return Ok(());
                }
                Some(filter)
            }
            Err(error) => {
                eprintln!("Error parsing filter mac: {}", error);
                return Ok(());
            }
        },
        None => None,
    };

    loop {
        if let Some(packet) = command_channel.next().await {
            if verbose {
                let pretty_string = packet.decode();
                // pretty print every packet in verbose mode.
                println!("{}", pretty_string);
            }

            match &packet {
                hci::EventPacket::InquiryResult { results, .. } => {
                    for result in results {
                        if mac.is_some() && !match_filter(&result.br_addr, mac.as_ref().unwrap()) {
                            continue;
                        }
                        println!(
                            "Found {:X}:{:X}:{:X}:{:X}:{:X}:{:X} Payload {}",
                            result.br_addr[0],
                            result.br_addr[1],
                            result.br_addr[2],
                            result.br_addr[3],
                            result.br_addr[4],
                            result.br_addr[5],
                            packet
                        );
                    }
                }
                hci::EventPacket::InquiryComplete { status_code, .. } => {
                    println!("Scan Complete: {}", status_code.name());
                    break;
                }
                _ => {}
            }
        } else {
            break;
        }
    }
    Ok(())
}

/// Parse program arguments, call the main loop, and log any unrecoverable errors.
#[fasync::run_singlethreaded]
async fn main() -> Result<(), Error> {
    let args: Args = argh::from_env();

    if args.verbose {
        println!("Opening device");
    }
    let command_channel = hci::open_default_device().context("Error opening HCI device")?;

    match args.subcommand {
        HciSubcommand::Raw(rawcmd) => raw_command(args.verbose, rawcmd, command_channel).await,
        HciSubcommand::Reset(_) => {
            let payload = ResetCommand::new().encode();
            basic_command(args.verbose, &payload[..], command_channel).await
        }
        HciSubcommand::BrEdrScan(scan) => {
            let payload = InquiryCommand::new(scan.timeout, scan.max_results).encode();
            command_channel.send_command_packet(&payload[..]).context("Error sending inquiry")?;
            scan_command(args.verbose, scan.filter, command_channel).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raw_packet_parsing_01() {
        assert_eq!(parse_payload(&["0xaaaa"]), Ok(vec![0xaa, 0xaa]));
    }

    #[test]
    fn test_raw_packet_parsing_02() {
        assert_eq!(parse_payload(&["0xaa", "0xaa"]), Ok(vec![0xaa, 0xaa]));
    }

    #[test]
    fn test_raw_packet_parsing_03() {
        assert_eq!(parse_payload(&["0xAa", "0XaA", "0x1234"]), Ok(vec![0xaa, 0xaa, 0x12, 0x34]));
    }

    #[test]
    fn test_raw_packet_parsing_04() {
        assert_eq!(parse_payload(&["0xaa", "0xaa", "0x1234"]), Ok(vec![0xaa, 0xaa, 0x12, 0x34]));
    }

    #[test]
    fn test_match_filter() {
        assert!(match_filter(&[0, 1, 2, 3, 4, 5], &[0]));
        assert!(match_filter(&[0, 1, 2, 3, 4, 5], &[0, 1]));
        assert!(!match_filter(&[0, 1, 2, 3, 4, 5], &[1]));
        assert!(!match_filter(&[0, 1, 2, 3, 4, 5], &[0, 2]));
        assert!(match_filter(&[0, 1, 2, 3, 4, 5], &[0, 1, 2, 3, 4, 5]));
        assert!(!match_filter(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff], &[0, 1, 2, 3, 4, 5]));
    }
}
