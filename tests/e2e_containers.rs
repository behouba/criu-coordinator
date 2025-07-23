use std::{
    collections::HashMap,
    fs,
    io::Write,
    net::TcpStream,
    process::{Child, Command},
    thread,
    time::Duration,
};

pub mod common;
use common::*;
use criu_coordinator::constants::{CONFIG_FILE};

const PODMAN_NETWORK_NAME: &str = "criu-coordinator-test-net";

struct PodmanProcess {
    id: String,
    container_name: String,
    host_pid: u32,
    archive_path: String,
}

struct TestGuard {
    server: Child,
    processes: Vec<PodmanProcess>,
}

impl Drop for TestGuard {
    fn drop(&mut self) {
        cleanup(&mut self.server, &mut self.processes);
    }
}

// === Helper Functions ===
fn is_podman_installed() -> bool {
    Command::new("podman")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
        .unwrap_or(false)
}
fn is_criu_installed() -> bool {
    Command::new("criu")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_cmd_and_assert(mut command: Command, context: &str) -> std::process::Output {
    println!("RUNNING: {:?}", command);
    let output = command.output().expect("Failed to execute command");
    assert!(
        output.status.success(),
        "{} failed. Stderr:\n{}",
        context,
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn get_host_pid(container_name: &str) -> u32 {
    let mut cmd = Command::new("podman");
    cmd.args(["inspect", "--format", "{{.State.Pid}}", container_name]);
    let output = run_cmd_and_assert(cmd, "Get container PID");
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .expect("Failed to parse PID")
}

fn get_logs(container_name: &str) -> String {
    let output = Command::new("podman")
        .args(["logs", container_name])
        .output()
        .expect("Failed to get logs");
    String::from_utf8_lossy(&output.stdout).to_string()
}

// === Setup & Teardown ===

fn setup(coordinator_port: u16, coordinator_addr: &str) -> Vec<PodmanProcess> {
    println!("\n--- Setting up Podman test environment ---");

    // Clean up previous runs
    let _ = Command::new("podman").args(["network", "rm", PODMAN_NETWORK_NAME, "--force"]).output();
    let _ = Command::new("podman").args(["stop", "--time=1", "criu-test-server", "criu-test-client"]).output();
    let _ = Command::new("podman").args(["rm", "--force", "criu-test-server", "criu-test-client"]).output();

    let mut create_net_cmd = Command::new("podman");
    create_net_cmd.args(["network", "create", PODMAN_NETWORK_NAME]);
    run_cmd_and_assert(create_net_cmd, "Create podman network");

    let security_opts = ["--cap-add=ALL", "--security-opt", "seccomp=unconfined"];
    let server_container_name = "criu-test-server";
    let client_container_name = "criu-test-client";

    let mut run_server_cmd = Command::new("podman");
    run_server_cmd
        .arg("run")
        .args(security_opts)
        .args([
            "--network",
            PODMAN_NETWORK_NAME,
            "--name",
            server_container_name,
            "-d",
            "docker.io/behouba/tcp-server",
            "/app/server",
            "8080",
        ]);
    run_cmd_and_assert(run_server_cmd, "Run tcp-server container");

    let mut run_client_cmd = Command::new("podman");
    run_client_cmd
        .arg("run")
        .args(security_opts)
        .args([
            "--network",
            PODMAN_NETWORK_NAME,
            "--name",
            client_container_name,
            "-d",
            "docker.io/behouba/tcp-client",
            "/app/client",
            server_container_name,
            "8080",
        ]);
    run_cmd_and_assert(run_client_cmd, "Run tcp-client container");

    thread::sleep(Duration::from_secs(3)); // Give containers time to start and connect

    let mut processes = Vec::new();
    let mut container_configs = HashMap::new();
    for (id, name) in [("tcp-server", server_container_name), ("tcp-client", client_container_name)].iter() {
        let host_pid = get_host_pid(name);
        processes.push(PodmanProcess {
            id: id.to_string(),
            container_name: name.to_string(),
            host_pid,
            archive_path: format!("/tmp/{}.tar.gz", name),
        });
        container_configs.insert(
            host_pid.to_string(),
            format!(r#""{}": {{ "id": "{}", "dependencies": {} }}"#, host_pid, id,
                if *id == "tcp-client" { r#"["tcp-server"]"# } else { "[]" }
            )
        );
    }

    // Create the central configuration file for criu-coordinator
    println!("--- Creating central coordinator configuration ---");
    let config_content = format!(
        r#"{{
        "address": "{}",
        "port": {},
        "log-file": "/tmp/criu-coordinator-client.log",
        "containers": {{
            {},
            {}
        }}
    }}"#,
        coordinator_addr,
        coordinator_port,
        container_configs.get(&processes.iter().find(|p| p.id == "tcp-server").unwrap().host_pid.to_string()).unwrap(),
        container_configs.get(&processes.iter().find(|p| p.id == "tcp-client").unwrap().host_pid.to_string()).unwrap(),
    );

    // This file will be found by the coordinator hook via its fallback search path
    fs::write(format!("/etc/criu/{}", CONFIG_FILE), &config_content).unwrap();
    println!("Wrote config to /etc/criu/{}", CONFIG_FILE);

    // Send the dependency information to the server before starting the checkpoint
    println!("--- Sending dependency map to coordinator server ---");
    let mut stream = TcpStream::connect(format!("{}:{}", coordinator_addr, coordinator_port)).unwrap();
    let dep_map = r#"{
        "id": "kubescr",
        "action": "add-dependencies",
        "dependencies": {
            "tcp-server": [],
            "tcp-client": ["tcp-server"]
        }
    }"#;
    stream.write_all(dep_map.as_bytes()).unwrap();
    stream.shutdown(std::net::Shutdown::Both).unwrap();


    processes
}

fn cleanup(server: &mut Child, processes: &mut [PodmanProcess]) {
    println!("\n--- Cleaning up test environment ---");
    fs::remove_file("/etc/criu/default.conf").ok();
    fs::remove_file(format!("/etc/criu/{}", CONFIG_FILE)).ok();
    for p in processes.iter() {
        fs::remove_file(&p.archive_path).ok();
    }

    let _ = server.kill();
    let _ = server.wait();
    println!("Killed coordinator server process.");

    for p in processes.iter_mut() {
        let _ = Command::new("podman").args(["stop", "--time=1", &p.container_name]).output();
        let _ = Command::new("podman").args(["rm", "--force", &p.container_name]).output();
        println!("Stopped and removed container {}", p.container_name);
    }
    let _ = Command::new("podman").args(["network", "rm", "--force", PODMAN_NETWORK_NAME]).output();
    println!("Removed podman network.");
    let _ = Command::new("make").arg("clean").current_dir("tests").status();
    println!("Cleanup complete.");
}

#[test]
#[ignore]
fn e2e_network_lock_with_podman() {
    assert!(is_root(), "This test must be run with root privileges.");
    assert!(is_criu_installed(), "CRIU command not found.");
    assert!(is_podman_installed(), "Podman command not found.");

    let coordinator_path = fs::canonicalize("target/debug/criu-coordinator")
        .expect("Binary not found.")
        .to_str()
        .unwrap()
        .to_owned();
    let coordinator_log = "/tmp/coordinator-test.log";
    let _ = fs::remove_file(coordinator_log); // Clean log from previous runs

    // Configure CRIU to use the coordinator as an action script for all commands
    fs::create_dir_all("/etc/criu").unwrap();
    fs::write("/etc/criu/default.conf", format!("action-script {}", coordinator_path)).unwrap();

    let mut make_cmd = Command::new("make");
    make_cmd.current_dir("tests");
    run_cmd_and_assert(make_cmd, "make test binaries");

    let port = pick_port();
    let coordinator_addr = "127.0.0.1";
    let server = spawn_server(coordinator_addr, port);
    assert!(server_ready(&format!("{}:{}", coordinator_addr, port), 20), "Server failed to start");

    let processes = setup(port, coordinator_addr);
    let guard = TestGuard { server, processes };
    let client_container_name = &guard.processes.iter().find(|p| p.id == "tcp-client").unwrap().container_name.clone();

    println!("\n--- Starting DUMP phase (concurrent) ---");
    let mut dump_handles = vec![];
    for p in &guard.processes {
        let mut cmd = Command::new("podman");
        cmd.arg("container")
            .arg("checkpoint")
            .args(["--tcp-established", "--leave-running", "--export", &p.archive_path, &p.container_name]);
        let p_id = p.id.clone();
        dump_handles.push(thread::spawn(move || (p_id, cmd.output().expect("podman checkpoint failed"))));
    }
    for handle in dump_handles {
        let (id, output) = handle.join().unwrap();
        assert!(
            output.status.success(),
            "Checkpoint failed for '{}'.\nStderr:\n{}",
            id,
            String::from_utf8_lossy(&output.stderr)
        );
        println!("Checkpoint successful for {}", id);
    }

    println!("\n--- Simulating Migration: Destroying original containers ---");
    for p in &guard.processes {
        let mut cmd = Command::new("podman");
        cmd.args(["rm", "--force", &p.container_name]);
        run_cmd_and_assert(cmd, &format!("Destroy original container {}", p.container_name));
    }
    
    // Podman starts the container but doesn't immediately show logs. Wait a bit.
    thread::sleep(Duration::from_secs(5));
    let logs_before_restore = get_logs(client_container_name); // Get logs just before we expect new output

    println!("\n--- Waiting for 2 seconds ---");
    thread::sleep(Duration::from_secs(2));

    println!("\n--- Starting RESTORE phase (concurrent) ---");
    let mut restore_handles = vec![];
    for p in &guard.processes {
        let mut cmd = Command::new("podman");
        let security_opts = ["--cap-add=ALL", "--security-opt", "seccomp=unconfined"];
        cmd.arg("container")
            .arg("restore")
            .args(security_opts)
            .args(["--network", PODMAN_NETWORK_NAME])
            .args(["--import", &p.archive_path, "--name", &p.container_name]);
        let p_id = p.id.clone();
        restore_handles.push(thread::spawn(move || (p_id, cmd.output().expect("podman restore failed"))));
    }
    for handle in restore_handles {
        let (id, output) = handle.join().unwrap();
        assert!(
            output.status.success(),
            "Restore failed for '{}'.\nStderr:\n{}",
            id,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    println!("Restore successful for both containers");

    println!("\n--- VERIFYING restored state ---");
    thread::sleep(Duration::from_secs(5));
    let logs_after_restore = get_logs(client_container_name);
    
    assert!(
        logs_after_restore.len() > logs_before_restore.len(),
        "No new log output after restore. Before:\n'{}'\nAfter:\n'{}'",
        logs_before_restore,
        logs_after_restore,
    );
    let new_logs = logs_after_restore.strip_prefix(&logs_before_restore).unwrap();
    assert!(new_logs.contains("ms"), "New log output did not contain expected messages.");
    println!("\nVerification successful: NEW log output was generated.");
}
