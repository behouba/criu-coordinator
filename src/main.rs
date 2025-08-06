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

struct ClientConfig {
    log_file: String,
    address: String,
    port: String,
    id: String,
    dependencies: String,
}

const CONFIG_KEY_ID: &str = "id";
const CONFIG_KEY_DEPS: &str = "dependencies";
const CONFIG_KEY_ADDR: &str = "address";
const CONFIG_KEY_PORT: &str = "port";
const CONFIG_KEY_LOG: &str = "log-file";
const CONFIG_KEY_CONTAINER_DEPS: &str = "dependencies";

/// Finds the container ID from a host PID by inspecting its cgroup.
fn find_container_id_from_pid(pid: u32) -> Result<String, String> {
    let cgroup_path = format!("/proc/{}/cgroup", pid);
    let cgroup_content = fs::read_to_string(&cgroup_path)
        .map_err(|e| format!("Failed to read {}: {}", cgroup_path, e))?;

    let mut last_found_id: Option<String> = None;
    for line in cgroup_content.lines() {
        if line.len() < 64 { continue; }
        for i in 0..=(line.len() - 64) {
            let potential_id = &line[i..i + 64];
            if potential_id.chars().all(|c| c.is_ascii_hexdigit()) {
                let is_start_boundary = i == 0 || !line.chars().nth(i - 1).unwrap().is_ascii_hexdigit();
                let is_end_boundary = (i + 64 == line.len()) || !line.chars().nth(i + 64).unwrap().is_ascii_hexdigit();
                if is_start_boundary && is_end_boundary {
                    last_found_id = Some(potential_id.to_string());
                }
            }
        }
    }
    
    last_found_id.ok_or_else(|| format!("Could not determine container ID from cgroup file for PID {}", pid))
}


/// Finds dependencies by matching the discovered ID as a prefix of a key in the map.
fn find_dependencies_in_central_config(
    dependencies_map: &HashMap<String, Vec<String>>,
    discovered_id: &str,
) -> Result<String, String> {
    let deps = dependencies_map.iter()
        .find(|(key, _)| discovered_id.starts_with(*key))
        .map(|(_, deps)| deps.join(":"))
        .ok_or_else(|| format!("No dependency entry found for container ID prefix matching '{}'", discovered_id))?;
    Ok(deps)
}

/// Writes the per-checkpoint configuration file into the image directory.
fn write_per_checkpoint_config(images_dir: &Path, id: &str, dependencies: &str) {
    let config_path = images_dir.join(CONFIG_FILE);
    let content = format!(
        "{{\n    \"id\": \"{}\",\n    \"dependencies\": \"{}\"\n}}",
        id,
        dependencies
    );
    fs::write(&config_path, content)
        .unwrap_or_else(|_| panic!("Failed to write per-checkpoint config to {:?}", config_path));
}

fn is_dump_action(action: &str) -> bool {
    matches!(action, ACTION_PRE_DUMP | ACTION_NETWORK_LOCK | ACTION_POST_DUMP | ACTION_PRE_STREAM)
}

fn is_restore_action(action: &str) -> bool {
    matches!(action, ACTION_PRE_RESTORE | ACTION_NETWORK_UNLOCK | ACTION_POST_RESTORE | ACTION_POST_RESUME)
}

fn load_config_file<P: AsRef<Path>>(images_dir: P, action: &str) -> ClientConfig {
    let images_dir = images_dir.as_ref();
    let local_config_file = images_dir.join(Path::new(CONFIG_FILE));
    let global_config_file = PathBuf::from("/etc/criu").join(Path::new(CONFIG_FILE));

    // Handle simple process workflow first (local config file is pre-created by user).
    if is_dump_action(action) && local_config_file.is_file() {
        let settings = Config::builder().add_source(config::File::from(local_config_file)).build().unwrap();
        let settings_map = settings.try_deserialize::<HashMap<String, String>>().unwrap();
        return ClientConfig {
            id: settings_map.get(CONFIG_KEY_ID).unwrap().clone(),
            dependencies: settings_map.get(CONFIG_KEY_DEPS).cloned().unwrap_or_default(),
            address: settings_map.get(CONFIG_KEY_ADDR).cloned().unwrap_or_else(|| DEFAULT_ADDRESS.to_string()),
            port: settings_map.get(CONFIG_KEY_PORT).cloned().unwrap_or_else(|| DEFAULT_PORT.to_string()),
            log_file: settings_map.get(CONFIG_KEY_LOG).cloned().unwrap_or_else(|| "-".to_string()),
        };
    }

    // --- Container and Restore Workflow ---
    if !global_config_file.is_file() {
        panic!("Global config file {:?} does not exist", global_config_file);
    }
    
    let central_settings = Config::builder().add_source(config::File::from(global_config_file)).build().unwrap();
    let central_map = central_settings.try_deserialize::<HashMap<String, config::Value>>().unwrap();
    
    let address = central_map.get(CONFIG_KEY_ADDR).map(|v| v.clone().into_string().unwrap()).unwrap_or_else(|| DEFAULT_ADDRESS.to_string());
    let port = central_map.get(CONFIG_KEY_PORT).map(|v| v.clone().into_string().unwrap()).unwrap_or_else(|| DEFAULT_PORT.to_string());
    let log_file = central_map.get(CONFIG_KEY_LOG).map(|v| v.clone().into_string().unwrap()).unwrap_or_else(|| "-".to_string());

    if is_dump_action(action) {
        let init_pid_str = env::var("CRTOOLS_INIT_PID").expect("CRTOOLS_INIT_PID not set");
        let init_pid: u32 = init_pid_str.parse().expect("Invalid PID");

        let container_deps_map: HashMap<String, Vec<String>> = central_map
            .get(CONFIG_KEY_CONTAINER_DEPS)
            .expect("'dependencies' map missing in central config")
            .clone().into_table().unwrap()
            .into_iter().map(|(k, v)| {
                let deps = v.into_array().unwrap().into_iter().map(|val| val.into_string().unwrap()).collect();
                (k, deps)
            }).collect();

        let container_id = find_container_id_from_pid(init_pid).unwrap();
        let dependencies = find_dependencies_in_central_config(&container_deps_map, &container_id).unwrap();
        
        // Only write the local config during the first dump action to avoid races.
        if action == ACTION_PRE_DUMP || action == ACTION_PRE_STREAM {
            write_per_checkpoint_config(images_dir, &container_id, &dependencies);
        }
        
        ClientConfig {
            id: container_id,
            dependencies,
            address,
            port,
            log_file,
        }
    } else { // Restore action
        if !local_config_file.is_file() {
            panic!("Restore action initiated, but no {} found in image directory {:?}", CONFIG_FILE, images_dir);
        }

        let local_settings = Config::builder().add_source(config::File::from(local_config_file)).build().unwrap();
        let local_map = local_settings.try_deserialize::<HashMap<String, String>>().unwrap();

        ClientConfig {
            id: local_map.get(CONFIG_KEY_ID).unwrap().clone(),
            dependencies: local_map.get(CONFIG_KEY_DEPS).cloned().unwrap_or_default(),
            address,
            port,
            log_file,
        }
    }
}

fn main() {
    if let Ok(action) = env::var(ENV_ACTION) {

        let images_dir = PathBuf::from(env::var(ENV_IMAGE_DIR)
            .unwrap_or_else(|_| panic!("Missing {} environment variable", ENV_IMAGE_DIR)));

        let client_config = load_config_file(&images_dir, &action);

        let enable_streaming = match action.as_str() {
            ACTION_PRE_STREAM => true,
            ACTION_PRE_DUMP => {
                match fs::symlink_metadata(images_dir.join(IMG_STREAMER_CAPTURE_SOCKET_NAME)) {
                    Ok(metadata) => {
                        if !metadata.file_type().is_socket() {
                            panic!("{} exists but is not a Unix socket", IMG_STREAMER_CAPTURE_SOCKET_NAME);
                        }
                        exit(0);
                    },
                    Err(_) => false
                }
            },
            _ => false,
        };
        
        if !is_dump_action(&action) && !is_restore_action(&action) {
            exit(0);
        }

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
