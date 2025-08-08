/*
 * Copyright (c) 2023 University of Oxford.
 * Copyright (c) 2023 Red Hat, Inc.
 * All rights reserved.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
 */

mod cli;
mod client;
mod server;
mod constants;
mod pipeline;
mod logger;

use constants::*;

use std::{env, path::PathBuf, process::{exit, Command, Output}, fs, os::unix::prelude::FileTypeExt};

use clap::{CommandFactory, Parser};
use clap_complete::{generate, Shell};
use std::io;
use log::*;

use cli::{Opts, Mode};
use client::run_client;
use server::run_server;
use logger::init_logger;

use crate::client::{load_config_file, is_dump_action, is_restore_action};

/// Runs an `ip` command inside the network namespace of a given PID.
fn run_ns_ip_command(pid: u32, args: &[&str]) -> std::io::Result<Output> {
    let netns_path = format!("/proc/{}/ns/net", pid);
    info!("Running in netns {}: ip {}", netns_path, args.join(" "));
    Command::new("nsenter")
        .arg(format!("--net={}", netns_path))
        .arg("ip")
        .args(args)
        .output()
}


/// Gets the name of the default network interface for a given PID's network namespace.
fn get_default_interface_name(pid: u32) -> Result<String, std::io::Error> {
    info!("Discovering default network interface for PID {}...", pid);
    let output = run_ns_ip_command(pid, &["-4", "route", "show", "default"])?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!("Failed to get default route for PID {}: {}", pid, stderr);
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "ip route command failed",
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut parts = stdout.split_whitespace();

    // The output is typically "default via 192.168.90.1 dev eth0"
    while let Some(part) = parts.next() {
        if part == "dev" {
            if let Some(iface) = parts.next() {
                info!("Found default interface for PID {}: {}", pid, iface);
                return Ok(iface.to_string());
            }
        }
    }

    error!("Could not parse default interface for PID {} from: {}", pid, stdout);
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "Default interface not found",
    ))
}

/// Handles the network locking and unlocking actions by manipulating the
/// container's default network interface via nsenter.
fn handle_network_action(action: &str, pid: u32) {
    let iface = match get_default_interface_name(pid) {
        Ok(name) => name,
        Err(e) => {
            error!("Could not determine default network interface for PID {}: {}", pid, e);
            exit(1);
        }
    };

    let result = match action {
        ACTION_NETWORK_LOCK => {
            info!("Performing network lock for PID {}: taking interface {} down.", pid, iface);
            run_ns_ip_command(pid, &["link", "set", &iface, "down"])
        }
        ACTION_NETWORK_UNLOCK => {
            info!("Performing network unlock for PID {}: bringing interface {} up.", pid, iface);
            run_ns_ip_command(pid, &["link", "set", &iface, "up"])
        }
        _ => return, // Not a network action we need to handle here.
    };

    match result {
        Ok(output) if output.status.success() => {
            info!("Network action '{}' for PID {} succeeded.", action, pid);
        }
        Ok(output) => {
            error!("Network action '{}' for PID {} failed with status: {}", action, pid, output.status);
            error!("Stderr: {}", String::from_utf8_lossy(&output.stderr));
            exit(1);
        }
        Err(e) => {
            error!("Network action '{}' for PID {} failed to execute: {}", action, pid, e);
            exit(1);
        }
    }
}

fn main() {
    if let Ok(action) = env::var(ENV_ACTION) {

        let images_dir = PathBuf::from(env::var(ENV_IMAGE_DIR)
            .unwrap_or_else(|_| panic!("Missing {} environment variable", ENV_IMAGE_DIR)));

        let client_config = load_config_file(&images_dir, &action);
        
        // Initialize logger early to capture network action logs
        init_logger(Some(&images_dir), client_config.get_log_file().to_string());

        // Perform the local network action *before* synchronizing with the server.
        // This requires the PID of the initial process in the container.
        if action == ACTION_NETWORK_LOCK || action == ACTION_NETWORK_UNLOCK {
            let init_pid_str = env::var(ENV_INIT_PID)
                .unwrap_or_else(|_| {
                    error!("FATAL: Missing {} for network action", ENV_INIT_PID);
                    exit(1);
                });
            let init_pid: u32 = init_pid_str.parse().expect("Invalid PID");
            handle_network_action(&action, init_pid);
        }

        // We need to explicitly handle all actions that should proceed to the client logic.
        // For anything else, we exit early.
        let enable_streaming = match action.as_str() {
            ACTION_PRE_STREAM => true,
            ACTION_PRE_DUMP => {
                match fs::symlink_metadata(images_dir.join(IMG_STREAMER_CAPTURE_SOCKET_NAME)) {
                    Ok(metadata) => {
                        if !metadata.file_type().is_socket() {
                            panic!("{} exists but is not a Unix socket", IMG_STREAMER_CAPTURE_SOCKET_NAME);
                        }
                        // If the stream socket exists, ignore CRIU's "pre-dump" action hook.
                        exit(0);
                    },
                    Err(_) => false
                }
            },
            ACTION_PRE_RESTORE |
            ACTION_POST_DUMP |
            ACTION_NETWORK_LOCK |
            ACTION_NETWORK_UNLOCK |
            ACTION_POST_RESTORE |
            ACTION_POST_RESUME => false,
            _ => exit(0),
        };

        // This check is redundant because of the match block above, but kept for safety.
        if !is_dump_action(&action) && !is_restore_action(&action) {
            exit(0)
        }

        run_client(
            client_config.get_address(),
            client_config.get_port().parse().unwrap(),
            client_config.get_id(),
            client_config.get_dependencies(),
            &action,
            &images_dir,
            enable_streaming
        );
        exit(0);
    }

    let opts = Opts::parse();

    match opts.mode {
        Mode::Completions { shell } => {
            let shell: Shell = shell.parse().expect("Invalid shell type");
            let mut cmd = Opts::command();
            generate(shell, &mut cmd, "criu-coordinator", &mut io::stdout());
        }

        Mode::Client { address, port, id, deps, action, images_dir, stream, log_file} => {
            init_logger(Some(&PathBuf::from(&images_dir)), log_file);
            run_client(&address, port, &id, &deps, &action, &PathBuf::from(images_dir), stream);
        },
        Mode::Server { address, port , max_retries, log_file} => {
            init_logger(None, log_file);
            run_server(&address, port, max_retries);
        }
    };
}
