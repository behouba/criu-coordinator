// The plan for this test is to:
//  - Use podman to create two pods one running a server container and the other running the client container
//  - Ensure the container are communicating over TCP
//  - Checkpoint the client and then the server while preserving the connection state with tcp-established
//  - Remove the pods and containers
//  - Try to recreate the pods and restore the containers
//  - Ensure the connection is resumed on both ends

use std::{
    env, fs::{self, File}, io::Write, process::{Child, Command, Stdio}, thread, time::{Duration, Instant}
};

mod common;
use common::*;

const NETWORK_NAME: &str = "criu-e2e-network";
const NETWORK_SUBNET: &str = "192.168.90.0/24";
const SERVER_IP: &str = "192.168.90.10";
const CLIENT_IP: &str = "192.168.90.20";
const SERVER_POD_NAME: &str = "tcp-server-pod-e2e";
const CLIENT_POD_NAME: &str = "tcp-client-pod-e2e";
const SERVER_CONTAINER_NAME: &str = "tcp-server-e2e";
const CLIENT_CONTAINER_NAME: &str = "tcp-client-e2e";
const SERVER_IMAGE_NAME: &str = "localhost/tcp-server-e2e:latest";
const CLIENT_IMAGE_NAME: &str = "localhost/tcp-client-e2e:latest";
// const CRIU_CONFIG_FILE_PATH: &str = "/etc/criu/default.conf";
const CRIU_CONFIG_DIR: &str = "/etc/criu";
const CRIU_CONFIG_FILE: &str = "/etc/criu/default.conf";
const CENTRAL_COORD_CONFIG_FILE: &str = "/etc/criu/criu-coordinator.json";
/// TestGuard ensures that the cleanup function is always called, even if the test panics.
struct PodTestGuard {
    server: Child,
}

impl Drop for PodTestGuard {
    fn drop(&mut self) {
        cleanup_pod_test(&mut self.server);
    }
}

/// Helper to run a command, print it, and assert that it was successful.
fn run_command(cmd: &mut Command, err_msg: &str) {
    println!("Executing: {cmd:?}");
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("{}: {}", err_msg, e));
    assert!(status.success(), "{} failed with status: {}", err_msg, status);
}

/// Helper to build a container image.
fn build_image(image_name: &str, dockerfile: &str, context: &str) {
    println!("Building image {image_name}...");
    run_command(
        Command::new("podman").args([
            "build",
            "--tag",
            image_name,
            "--file",
            dockerfile,
            context,
        ]),
        &format!("Failed to build image {image_name}"),
    );
}

/// Gets the full logs (stdout and stderr) for a container.
fn get_logs(container_name: &str) -> String {
    let output = Command::new("podman")
        .args(["logs", container_name])
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("Failed to get logs");
    
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("STDOUT:\n{stdout}\nSTDERR:\n{stderr}")
}


fn is_podman_installed() -> bool {
    Command::new("podman").arg("--version").output().map(|out| out.status.success()).unwrap_or(false)
}


/// Parses the last integer counter from the logs.
fn get_last_counter(logs: &str, pattern: &str) -> Option<i32> {
    logs.lines()
        .rev()
        .find(|line| line.contains(pattern))
        .and_then(|line| line.split_whitespace().last())
        .and_then(|num_str| num_str.parse::<i32>().ok())
}


/// Sets up the entire test environment: binaries, images, network, pods, and containers.
fn setup_pod_test() {
    println!("\n--- Setting up Podman test environment ---");

    let make_status = Command::new("make")
        .current_dir("tests")
        .status()
        .expect("Failed to run `make` in tests directory");
    assert!(make_status.success(), "make command failed");

    let tests_dir = env::current_dir().unwrap().join("tests");
    let server_dockerfile_path = tests_dir.join("Dockerfile.tcp-server");
    fs::write(&server_dockerfile_path, "FROM fedora:latest\nCOPY tcp-server /usr/local/bin/\nCMD [\"/usr/local/bin/tcp-server\", \"8080\"]")
        .expect("Failed to write server Dockerfile");
    
    let client_dockerfile_path = tests_dir.join("Dockerfile.tcp-client");
    fs::write(&client_dockerfile_path, "FROM fedora:latest\nCOPY tcp-client /usr/local/bin/\nENTRYPOINT [\"/usr/local/bin/tcp-client\"]")
        .expect("Failed to write client Dockerfile");

    build_image(SERVER_IMAGE_NAME, server_dockerfile_path.to_str().unwrap(), tests_dir.to_str().unwrap());
    build_image(CLIENT_IMAGE_NAME, client_dockerfile_path.to_str().unwrap(), tests_dir.to_str().unwrap());

    run_command(
        Command::new("podman").args(["network", "create", "--subnet", NETWORK_SUBNET, NETWORK_NAME]),
        "Failed to create podman network",
    );

    run_command(
        Command::new("podman").args(["pod", "create", "--name", SERVER_POD_NAME, "--network", NETWORK_NAME, "--ip", SERVER_IP]),
        "Failed to create server pod",
    );
    run_command(
        Command::new("podman").args(["pod", "create", "--name", CLIENT_POD_NAME, "--network", NETWORK_NAME, "--ip", CLIENT_IP]),
        "Failed to create client pod",
    );

    run_command(
        Command::new("podman").args(["run", "-dt", "--pod", SERVER_POD_NAME, "--name", SERVER_CONTAINER_NAME, SERVER_IMAGE_NAME]),
        "Failed to run server container",
    );
    thread::sleep(Duration::from_secs(2));

    run_command(
        Command::new("podman").args([
            "run", "-dt", "--pod", CLIENT_POD_NAME, "--name", CLIENT_CONTAINER_NAME,
            CLIENT_IMAGE_NAME, SERVER_IP, "8080",
        ]),
        "Failed to run client container",
    );

    println!("Waiting for TCP connection to be established...");
    let start = Instant::now();
    let timeout = Duration::from_secs(15);
    loop {
        let server_logs = get_logs(SERVER_CONTAINER_NAME);
        let client_logs = get_logs(CLIENT_CONTAINER_NAME);

        let server_connected = server_logs.contains("New connection");
        let client_connected = client_logs.contains("Connected");

        if server_connected && client_connected {
            println!("TCP connection verified.");
            return;
        }

        if start.elapsed() > timeout {
            panic!("Timed out waiting for TCP connection.\nServer logs:\n{}\n\nClient logs:\n{}", server_logs, client_logs);
        }
        thread::sleep(Duration::from_secs(1));
    }
}

/// Cleans up all resources created during the test.
fn cleanup_pod_test(server: &mut Child) {
    println!("\n--- Cleaning up Podman test environment ---");
    let _ = server.kill();
    let _ = server.wait();
    println!("Killed server process.");
    
    let _ = Command::new("podman").args(["pod", "rm", "--force", SERVER_POD_NAME]).status();
    let _ = Command::new("podman").args(["pod", "rm", "--force", CLIENT_POD_NAME]).status();
    let _ = Command::new("podman").args(["network", "rm", NETWORK_NAME]).status();
    let _ = Command::new("podman").args(["image", "rm", SERVER_IMAGE_NAME]).status();
    let _ = Command::new("podman").args(["image", "rm", CLIENT_IMAGE_NAME]).status();
    let _ = Command::new("make").arg("clean").current_dir("tests").status();
    let _ = fs::remove_file(format!("/tmp/{SERVER_CONTAINER_NAME}.tar.gz"));
    let _ = fs::remove_file(format!("/tmp/{CLIENT_CONTAINER_NAME}.tar.gz"));
    let _ = fs::remove_file("tests/Dockerfile.tcp-server");
    let _ = fs::remove_file("tests/Dockerfile.tcp-client");
    let _ = fs::remove_file(CRIU_CONFIG_FILE);
    println!("Podman cleanup complete.");
}

/// Gets the full, untruncated container ID.
fn get_container_id(container_name: &str) -> String {
    let output = Command::new("podman")
        .args(["inspect", "--format", "{{.Id}}", container_name])
        .output()
        .expect("Failed to inspect container");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[test]
#[ignore]
fn e2e_pod_tcp_checkpoint_restore() {
    if !is_root() {
        panic!("This test must be run with root privileges for 'podman checkpoint'.");
    }
    if !is_podman_installed() {
        panic!("Podman command not found in PATH.");
    }
    if !is_criu_installed() {
        panic!("CRIU is a dependency for 'podman checkpoint' and was not found in PATH.");
    }

    // cleanup_pod_test();

    // let coordinator_port = pick_port();
    // let coordinator_addr = format!("127.0.0.1:{coordinator_port}");
    // let coordinator_server = spawn_server(coordinator_port);
    // assert!(
    //     server_ready(&coordinator_addr, 20),
    //     "Coordinator server failed to start at {}",
    //     coordinator_addr
    // );


    // let _guard = PodTestGuard { server: coordinator_server};

    // Add criu-coordinator as a default action-script to be used by CRIU
    // let criu_dir = Path::new(CRIU_CONFIG_FILE_PATH).parent().unwrap();

    // fs::create_dir_all(criu_dir).unwrap();

    // File::create(CRIU_CONFIG_FILE_PATH).unwrap().write_all(format!("action-script={}", CRIU_COORDINATOR_PATH).as_bytes()).unwrap();


    let coordinator_port = pick_port();
    let coordinator_addr = format!("127.0.0.1:{coordinator_port}");
    let coordinator_server = spawn_server(coordinator_port);
    assert!(
        server_ready(&coordinator_addr, 20),
        "Coordinator server failed to start at {}",
        coordinator_addr
    );

    let _guard = PodTestGuard { server: coordinator_server};

    // cleanup_pod_test(server);

    setup_pod_test();

    // --- Create Central Coordinator Config ---
    let client_id = get_container_id(CLIENT_CONTAINER_NAME);
    let server_id = get_container_id(SERVER_CONTAINER_NAME);
    println!("Client container ID: {}", client_id);
    println!("Server container ID: {}", server_id);

    fs::create_dir_all(CRIU_CONFIG_DIR).unwrap();
    let central_config_content = format!(
        r#"{{
            "address": "127.0.0.1",
            "port": {},
            "dependencies": {{
                "{}": ["{}"],
                "{}": []
            }}
        }}"#,
        coordinator_port,
        &client_id, // Using short IDs for convenience in config
        &server_id,
        &server_id
    );
    let mut config_file = File::create(CENTRAL_COORD_CONFIG_FILE).unwrap();
    config_file.write_all(central_config_content.as_bytes()).unwrap();
    println!("Created central config at {}", CENTRAL_COORD_CONFIG_FILE);

    // --- Create CRIU default.conf to set the action script globally ---
    let coordinator_path = fs::canonicalize(CRIU_COORDINATOR_PATH)
        .expect("Could not find criu-coordinator binary")
        .to_str().unwrap().to_owned();
    let criu_conf_content = format!("action-script={}", coordinator_path);
    let mut criu_conf_file = File::create(CRIU_CONFIG_FILE).unwrap();
    criu_conf_file.write_all(criu_conf_content.as_bytes()).unwrap();
    println!("Created CRIU config at {} to use coordinator as action script", CRIU_CONFIG_FILE);



    // Let the applications exchange a few messages before checkpointing
    println!("Allowing initial communication...");
    thread::sleep(Duration::from_secs(3));
    let client_logs_before = get_logs(CLIENT_CONTAINER_NAME);
    let server_logs_before = get_logs(SERVER_CONTAINER_NAME);
    
    let client_counter_before = get_last_counter(&client_logs_before, "Client <- Server:")
        .expect("Could not find initial client counter in logs");
    let server_counter_before = get_last_counter(&server_logs_before, "Server -> Client:")
        .expect("Could not find initial server counter in logs");
    
    println!("Counters before checkpoint: Client={client_counter_before}, Server={server_counter_before}");

    println!("\n--- Starting CHECKPOINT phase ---");
    let server_checkpoint_file = format!("/tmp/{}.tar.gz", SERVER_CONTAINER_NAME);
    let client_checkpoint_file = format!("/tmp/{}.tar.gz", CLIENT_CONTAINER_NAME);

    // --- CONCURRENT CHECKPOINT ---
    let mut checkpoint_handles = vec![];

    // Create clones of the String paths to be moved into the threads.
    let client_checkpoint_file_clone = client_checkpoint_file.clone();
    let client_handle = thread::spawn(move || {
        let mut cmd = Command::new("podman");
        cmd.args(["container", "checkpoint", CLIENT_CONTAINER_NAME, "--tcp-established", "--leave-running", "-e", &client_checkpoint_file_clone]);
        println!("Executing: {:?}", cmd);
        cmd.output().expect("Failed to checkpoint client container")
    });
    checkpoint_handles.push(("Client", client_handle));

    let server_checkpoint_file_clone = server_checkpoint_file.clone();
    let server_handle = thread::spawn(move || {
        let mut cmd = Command::new("podman");
        cmd.args(["container", "checkpoint", SERVER_CONTAINER_NAME, "--tcp-established", "--leave-running", "-e", &server_checkpoint_file_clone]);
        println!("Executing: {:?}", cmd);
        cmd.output().expect("Failed to checkpoint server container")
    });
    checkpoint_handles.push(("Server", server_handle));

    for (name, handle) in checkpoint_handles {
        let output = handle.join().unwrap();
        assert!(output.status.success(), "Failed to checkpoint {} container.\nStdout:\n{}\nStderr:\n{}",
            name, String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }

    // --- TEARDOWN AND RECREATE PODS FOR RESTORE ---
    println!("\n--- Tearing down old pods and network for restore ---");
    run_command(
        Command::new("podman").args(["stop", CLIENT_CONTAINER_NAME]),
        "Failed to stop client container",
    );
    run_command(
        Command::new("podman").args(["stop", SERVER_CONTAINER_NAME]),
        "Failed to stop server container",
    );
    run_command(
        Command::new("podman").args(["pod", "rm", "-f", CLIENT_POD_NAME]),
        "Failed to remove client pod",
    );
    run_command(
        Command::new("podman").args(["pod", "rm", "-f", SERVER_POD_NAME]),
        "Failed to remove server pod",
    );
    // The network is implicitly used by the pods for restore, so we don't remove it yet.
    // We recreate the pods to ensure a clean state for the restore operation.
    run_command(
        Command::new("podman").args(["pod", "create", "--name", SERVER_POD_NAME, "--network", NETWORK_NAME, "--ip", SERVER_IP]),
        "Failed to re-create server pod for restore",
    );
    run_command(
        Command::new("podman").args(["pod", "create", "--name", CLIENT_POD_NAME, "--network", NETWORK_NAME, "--ip", CLIENT_IP]),
        "Failed to re-create client pod for restore",
    );

    println!("\n--- Starting RESTORE phase ---");
    let mut restore_handles = vec![];
    
    // The original `client_checkpoint_file` is still available in this scope.
    // We clone it again for the restore thread.
    let client_checkpoint_file_clone = client_checkpoint_file.clone();
    let client_restore_handle = thread::spawn(move || {
        let mut cmd = Command::new("podman");
        cmd.args(["container", "restore", "--tcp-established", "--pod", CLIENT_POD_NAME, "-i", &client_checkpoint_file_clone]);
        println!("Executing: {:?}", cmd);
        cmd.output().expect("Failed to restore client container")
    });
    restore_handles.push(("Client", client_restore_handle));

    // Same for the server file.
    let server_checkpoint_file_clone = server_checkpoint_file.clone();
    let server_restore_handle = thread::spawn(move || {
        let mut cmd = Command::new("podman");
        cmd.args(["container", "restore", "--tcp-established", "--pod", SERVER_POD_NAME, "-i", &server_checkpoint_file_clone]);
        println!("Executing: {:?}", cmd);
        cmd.output().expect("Failed to restore server container")
    });
    restore_handles.push(("Server", server_restore_handle));

    for (name, handle) in restore_handles {
        let output = handle.join().unwrap();
        assert!(output.status.success(), "Failed to restore {} container.\nStdout:\n{}\nStderr:\n{}",
            name, String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }

    println!("\n--- Verifying connection after restore ---");

    // Let the applications run for a few seconds to exchange more messages
    println!("Waiting for post-restore communication...");
    thread::sleep(Duration::from_secs(4));

    // Get the complete logs after the restore
    let client_logs_after = get_logs(CLIENT_CONTAINER_NAME);
    let server_logs_after = get_logs(SERVER_CONTAINER_NAME);

    println!("\n--- Post-Restore Server Logs ---\n{server_logs_after}");
    println!("\n--- Post-Restore Client Logs ---\n{client_logs_after}");


    // Check for any new connection errors post-restore
    assert!(
        !client_logs_after.contains("CLIENT_ERROR"),
        "Client logs contain connection errors after restore."
    );
    assert!(
        !server_logs_after.contains("Can't write socket"),
        "Server logs contain connection errors after restore."
    );
    println!("Verified: No new connection errors in logs.");


    // Get the final counter values
    let client_counter_after = get_last_counter(&client_logs_after, "Client <- Server:")
        .expect("Could not find client counter in logs after restore");
    let server_counter_after = get_last_counter(&server_logs_after, "Server -> Client:")
        .expect("Could not find server counter in logs after restore");

    println!("Counters after restore: Client saw {}, Server sent {}", client_counter_after, server_counter_after);


    // Verify that the counters have meaningfully increased since before the checkpoint.
    //    A small increase proves the connection was re-established and data flowed.
    assert!(
        client_counter_after > client_counter_before + 1,
        "Client counter did not increase sufficiently after restore. Before: {}, After: {}",
        client_counter_before, client_counter_after
    );
    assert!(
        server_counter_after > server_counter_before + 1,
        "Server counter did not increase sufficiently after restore. Before: {}, After: {}",
        server_counter_before, server_counter_after
    );
    println!("Verified: Communication resumed and counters increased.");


    println!("\nTest completed successfully.");
}
