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
use config::Config;
use log::info;
use std::collections::HashMap;
use std::{env, path::PathBuf, process::exit, fs, os::unix::prelude::FileTypeExt};
use std::path::Path;

use clap::{CommandFactory, Parser};
use clap_complete::{generate, Shell};
use std::io;

use cli::{Opts, Mode, DEFAULT_ADDRESS, DEFAULT_PORT};
use client::run_client;
use server::run_server;
use logger::init_logger;
use serde::Deserialize;

struct ClientConfig {
    log_file: String,
    address: String,
    port: String,
    id: String,
    dependencies: String,
}

#[derive(Deserialize, Debug)]
struct ContainerConfig {
    id: String,
    #[serde(default)]
    dependencies: Vec<String>,
}

// FIX: Add derive macro
#[derive(Deserialize, Debug)]
struct GlobalConfig {
    address: String,
    port: u16,
    #[serde(rename = "log-file", default = "default_log_file")]
    log_file: String,
    containers: HashMap<String, ContainerConfig>, // Key is Host PID
}

fn default_log_file() -> String {
    "-".to_string()
}


// const CONFIG_KEY_ID: &str = "id";
// const CONFIG_KEY_DEPS: &str = "dependencies";
// const CONFIG_KEY_ADDR: &str = "address";
// const CONFIG_KEY_PORT: &str = "port";
// const CONFIG_KEY_LOG: &str = "log-file";


fn load_config_file<P: AsRef<Path>>(images_dir: P, init_pid: Option<&str>) -> ClientConfig {
    let images_dir = images_dir.as_ref();
    let local_config_path = images_dir.join(Path::new(CONFIG_FILE));
    let global_config_path = PathBuf::from("/etc/criu").join(Path::new(CONFIG_FILE));

    let config_file_path = if local_config_path.is_file() {
        info!("Loading local config from {:?}", local_config_path);
        local_config_path
    } else if global_config_path.is_file() {
        info!("Loading global config from {:?}", global_config_path);
        global_config_path
    } else {
        panic!("No config file found at {:?} or {:?}", local_config_path, global_config_path);
    };

    let settings = Config::builder()
        .add_source(config::File::from(config_file_path))
        .build()
        .expect("Failed to build config");

    // Try to parse as GlobalConfig first
    if let Ok(global_config) = settings.clone().try_deserialize::<GlobalConfig>() {
        if !global_config.containers.is_empty() {
            let init_pid_str = init_pid.expect("INIT_PID is required for global container config");
            let container_config = global_config
                .containers
                .get(init_pid_str)
                .unwrap_or_else(|| panic!("Configuration for PID {} not found in containers map", init_pid_str));

            return ClientConfig {
                log_file: global_config.log_file.to_string(),
                address: global_config.address.to_string(),
                port: global_config.port.to_string(),
                id: container_config.id.to_string(),
                dependencies: container_config.dependencies.join(":"),
            };
        }
    }

    // Fallback to simple key-value parsing for process-based tests
    info!("Falling back to simple key-value config parsing.");
    let settings_map = settings.try_deserialize::<HashMap<String, String>>().expect("Failed to parse simple config");

    let id = settings_map.get("id").expect("id missing in config file").to_string();
    let dependencies = settings_map.get("dependencies").map_or(String::new(), |s| s.clone());
    let address = settings_map.get("address").map_or(DEFAULT_ADDRESS.to_string(), |s| s.clone());
    let port = settings_map.get("port").map_or(DEFAULT_PORT.to_string(), |s| s.clone());
    let log_file = settings_map.get("log-file").map_or("-".to_string(), |s| s.clone());

    ClientConfig {
        log_file,
        address,
        port,
        id,
        dependencies,
    }
}

fn main() {
    if let Ok(action) = env::var(ENV_ACTION) {

        let images_dir = PathBuf::from(env::var(ENV_IMAGE_DIR)
            .unwrap_or_else(|_| panic!("Missing {} environment variable", ENV_IMAGE_DIR)));

        // let client_config = load_config_file(&images_dir);

        let init_pid = env::var(ENV_INIT_PID).ok();

        let client_config = load_config_file(&images_dir, init_pid.as_deref());
        // if action == ACTION_NETWORK_LOCK || action == ACTION_NETWORK_UNLOCK {
        //     if let Ok(init_pid) = env::var(ENV_INIT_PID) {
        //         info!("CRTOOLS_INIT_PID: {}", init_pid);
        //     } else {
        //         info!("CRTOOLS_INIT_PID not found in environement.");
        //     }
        // }

        info!("action: {}", action);

        // Ignore all action hooks other than "pre-stream", "pre-dump" and "pre-restore".
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
            ACTION_POST_DUMP => false,
            ACTION_PRE_RESTORE => false,
            ACTION_NETWORK_LOCK => false,
            ACTION_NETWORK_UNLOCK => false,
            _ => exit(0)
        };

        init_logger(Some(&images_dir), client_config.log_file);

        run_client(
            &client_config.address,
            client_config.port.parse().unwrap(),
            &client_config.id,
            &client_config.dependencies,
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
