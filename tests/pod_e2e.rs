// // The plan for this test is to:
// //  - Use podman to create two pods one running a server container and the other running the client container
// //  - Ensure the container are communicating over TCP
// //  - Checkpoint the client and then the server while preserving the connection state with tcp-established
// //  - Remove the pods and containers
// //  - Try to recreate the pods and restore the containers
// //  - Ensure the connection is resumed on both ends

// use std::{
//     env, fs,
//     path::{Path, PathBuf},
//     process::{Command, Stdio},
//     thread,
//     time::{Duration, Instant},
// };

// mod common;
// use common::*;

// const NETWORK_NAME: &str = "criu-e2e-network";
// const NETWORK_SUBNET: &str = "192.168.90.0/24";
// const SERVER_IP: &str = "192.168.90.10";
// const SERVER_POD_NAME: &str = "tcp-server-pod-e2e";
// const CLIENT_POD_NAME: &str = "tcp-client-pod-e2e";
// const SERVER_CONTAINER_NAME: &str = "tcp-server-e2e";
// const CLIENT_CONTAINER_NAME: &str = "tcp-client-e2e";
// const SERVER_IMAGE_NAME: &str = "localhost/tcp-server-e2e:latest";
// const CLIENT_IMAGE_NAME: &str = "localhost/tcp-client-e2e:latest";

// /// TestGuard ensures that the cleanup function is always called, even if the test panics.
// struct PodmanTestGuard {
//     _marker: (),
// }

// impl Drop for PodmanTestGuard {
//     fn drop(&mut self) {
//         cleanup_pod_test();
//     }
// }

// /// Helper to run a command, print it, and assert that it was successful.
// fn run_command(cmd: &mut Command, err_msg: &str) {
//     println!("Executing: {:?}", cmd);
//     let status = cmd
//         .status()
//         .unwrap_or_else(|e| panic!("{}: {}", err_msg, e));
//     assert!(status.success(), "{} failed with status: {}", err_msg, status);
// }

// /// Helper to build a container image.
// fn build_image(image_name: &str, dockerfile: &str, context: &str) {
//     println!("Building image {}...", image_name);
//     run_command(
//         Command::new("podman").args([
//             "build",
//             "--tag",
//             image_name,
//             "--file",
//             dockerfile,
//             context,
//         ]),
//         &format!("Failed to build image {}", image_name),
//     );
// }

// /// Gets the full logs (stdout and stderr) for a container.
// fn get_logs(container_name: &str) -> String {
//     let output = Command::new("podman")
//         .args(["logs", container_name])
//         .stderr(Stdio::piped())
//         .stdout(Stdio::piped())
//         .output()
//         .expect("Failed to get logs");
    
//     let stdout = String::from_utf8_lossy(&output.stdout);
//     let stderr = String::from_utf8_lossy(&output.stderr);
//     format!("STDOUT:\n{}\nSTDERR:\n{}", stdout, stderr)
// }


// fn is_podman_installed() -> bool {
//     Command::new("podman").arg("--version").output().map(|out| out.status.success()).unwrap_or(false)
// }


// /// Parses the last integer counter from the logs.
// fn get_last_counter(logs: &str, pattern: &str) -> Option<i32> {
//     logs.lines()
//         .rev()
//         .find(|line| line.contains(pattern))
//         .and_then(|line| line.split_whitespace().last())
//         .and_then(|num_str| num_str.parse::<i32>().ok())
// }


// /// Sets up the entire test environment: binaries, images, network, pods, and containers.
// fn setup_pod_test() {
//     println!("\n--- Setting up Podman test environment ---");

//     let make_status = Command::new("make")
//         .current_dir("tests")
//         .status()
//         .expect("Failed to run `make` in tests directory");
//     assert!(make_status.success(), "make command failed");

//     let tests_dir = env::current_dir().unwrap().join("tests");
//     let server_dockerfile_path = tests_dir.join("Dockerfile.tcp-server");
//     fs::write(&server_dockerfile_path, "FROM fedora:latest\nCOPY tcp-server /usr/local/bin/\nCMD [\"/usr/local/bin/tcp-server\", \"8080\"]")
//         .expect("Failed to write server Dockerfile");
    
//     let client_dockerfile_path = tests_dir.join("Dockerfile.tcp-client");
//     fs::write(&client_dockerfile_path, "FROM fedora:latest\nCOPY tcp-client /usr/local/bin/\nENTRYPOINT [\"/usr/local/bin/tcp-client\"]")
//         .expect("Failed to write client Dockerfile");

//     build_image(SERVER_IMAGE_NAME, server_dockerfile_path.to_str().unwrap(), tests_dir.to_str().unwrap());
//     build_image(CLIENT_IMAGE_NAME, client_dockerfile_path.to_str().unwrap(), tests_dir.to_str().unwrap());

//     run_command(
//         Command::new("podman").args(["network", "create", "--subnet", NETWORK_SUBNET, NETWORK_NAME]),
//         "Failed to create podman network",
//     );

//     run_command(
//         Command::new("podman").args(["pod", "create", "--name", SERVER_POD_NAME, "--network", NETWORK_NAME, "--ip", SERVER_IP]),
//         "Failed to create server pod",
//     );
//     run_command(
//         Command::new("podman").args(["pod", "create", "--name", CLIENT_POD_NAME, "--network", NETWORK_NAME]),
//         "Failed to create client pod",
//     );

//     run_command(
//         Command::new("podman").args(["run", "-dt", "--pod", SERVER_POD_NAME, "--name", SERVER_CONTAINER_NAME, SERVER_IMAGE_NAME]),
//         "Failed to run server container",
//     );
//     thread::sleep(Duration::from_secs(2));

//     run_command(
//         Command::new("podman").args([
//             "run", "-dt", "--pod", CLIENT_POD_NAME, "--name", CLIENT_CONTAINER_NAME,
//             CLIENT_IMAGE_NAME, SERVER_IP, "8080",
//         ]),
//         "Failed to run client container",
//     );

//     println!("Waiting for TCP connection to be established...");
//     let start = Instant::now();
//     let timeout = Duration::from_secs(15);
//     loop {
//         let server_logs = get_logs(SERVER_CONTAINER_NAME);
//         let client_logs = get_logs(CLIENT_CONTAINER_NAME);

//         let server_connected = server_logs.contains("New connection established.");
//         let client_connected = client_logs.contains("Connected.");

//         if server_connected && client_connected {
//             println!("TCP connection verified.");
//             return;
//         }

//         if start.elapsed() > timeout {
//             panic!("Timed out waiting for TCP connection.\nServer logs:\n{}\n\nClient logs:\n{}", server_logs, client_logs);
//         }
//         thread::sleep(Duration::from_secs(1));
//     }
// }

// /// Cleans up all resources created during the test.
// fn cleanup_pod_test() {
//     println!("\n--- Cleaning up Podman test environment ---");
//     let _ = Command::new("podman").args(["pod", "rm", "--force", SERVER_POD_NAME]).status();
//     let _ = Command::new("podman").args(["pod", "rm", "--force", CLIENT_POD_NAME]).status();
//     let _ = Command::new("podman").args(["network", "rm", NETWORK_NAME]).status();
//     let _ = Command::new("podman").args(["image", "rm", SERVER_IMAGE_NAME]).status();
//     let _ = Command::new("podman").args(["image", "rm", CLIENT_IMAGE_NAME]).status();
//     let _ = Command::new("make").arg("clean").current_dir("tests").status();
//     let _ = fs::remove_file(format!("/tmp/{}.tar.gz", SERVER_CONTAINER_NAME));
//     let _ = fs::remove_file(format!("/tmp/{}.tar.gz", CLIENT_CONTAINER_NAME));
//     let _ = fs::remove_file("tests/Dockerfile.tcp-server");
//     let _ = fs::remove_file("tests/Dockerfile.tcp-client");
//     println!("Podman cleanup complete.");
// }

// #[test]
// #[ignore]
// fn e2e_pod_tcp_checkpoint_restore() {
//     if !is_root() {
//         panic!("This test must be run with root privileges for 'podman checkpoint'.");
//     }
//     if !is_podman_installed() {
//         panic!("Podman command not found in PATH.");
//     }
//     if !is_criu_installed() {
//         panic!("CRIU is a dependency for 'podman checkpoint' and was not found in PATH.");
//     }

//     cleanup_pod_test();
//     let _guard = PodmanTestGuard { _marker: () };
//     setup_pod_test();

//     // Let the applications exchange a few messages before checkpointing
//     println!("Allowing initial communication...");
//     thread::sleep(Duration::from_secs(3));
//     let client_logs_before = get_logs(CLIENT_CONTAINER_NAME);
//     let server_logs_before = get_logs(SERVER_CONTAINER_NAME);
    
//     let client_counter_before = get_last_counter(&client_logs_before, "Client -> Server:")
//         .expect("Could not find initial client counter in logs");
//     let server_counter_before = get_last_counter(&server_logs_before, "Server -> Client:")
//         .expect("Could not find initial server counter in logs");
    
//     println!("Counters before checkpoint: Client={}, Server={}", client_counter_before, server_counter_before);

//     println!("\n--- Starting CHECKPOINT phase ---");
//     let server_checkpoint_file = format!("/tmp/{}.tar.gz", SERVER_CONTAINER_NAME);
//     let client_checkpoint_file = format!("/tmp/{}.tar.gz", CLIENT_CONTAINER_NAME);

//     run_command(Command::new("podman").args(["container", "checkpoint", CLIENT_CONTAINER_NAME, "--tcp-established", "--leave-running", "-e", &client_checkpoint_file]), "Failed to checkpoint client container");
//     run_command(Command::new("podman").args(["container", "checkpoint", SERVER_CONTAINER_NAME, "--tcp-established", "--leave-running", "-e", &server_checkpoint_file]), "Failed to checkpoint server container");

//     println!("\n--- Tearing down original pods and network ---");
//     run_command(Command::new("podman").args(["pod", "rm", "-f", SERVER_POD_NAME]), "Failed to remove server pod");
//     run_command(Command::new("podman").args(["pod", "rm", "-f", CLIENT_POD_NAME]), "Failed to remove client pod");
//     run_command(Command::new("podman").args(["network", "rm", NETWORK_NAME]), "Failed to remove network");

//     println!("\n--- Recreating pods and network for RESTORE ---");
//     run_command(Command::new("podman").args(["network", "create", "--subnet", NETWORK_SUBNET, NETWORK_NAME]), "Failed to recreate podman network");
//     run_command(Command::new("podman").args(["pod", "create", "--name", SERVER_POD_NAME, "--network", NETWORK_NAME, "--ip", SERVER_IP]), "Failed to recreate server pod");
//     run_command(Command::new("podman").args(["pod", "create", "--name", CLIENT_POD_NAME, "--network", NETWORK_NAME]), "Failed to recreate client pod");

//     println!("\n--- Starting RESTORE phase ---");
//     run_command(Command::new("podman").args(["container", "restore", "--tcp-established", "--pod", SERVER_POD_NAME, "-i", &server_checkpoint_file]), "Failed to restore server container");
//     run_command(Command::new("podman").args(["container", "restore", "--tcp-established", "--pod", CLIENT_POD_NAME, "-i", &client_checkpoint_file]), "Failed to restore client container");

//     println!("\n--- Verifying connection after restore ---");
//     thread::sleep(Duration::from_secs(3));
//     let client_logs_after = get_logs(CLIENT_CONTAINER_NAME);
//     let server_logs_after = get_logs(SERVER_CONTAINER_NAME);

//     let client_counter_after = get_last_counter(&client_logs_after, "Client -> Server:")
//         .expect("Could not find client counter in logs after restore");
//     let server_counter_after = get_last_counter(&server_logs_after, "Server -> Client:")
//         .expect("Could not find server counter in logs after restore");
    
//     println!("Logs after: Client={}, Server={}", client_logs_after, server_logs_after);
//     println!("Counters after restore: Client={}, Server={}", client_counter_after, server_counter_after);

//     assert!(client_counter_after == client_counter_before, "Client counter did not increase after restore.");
//     assert!(server_counter_after == server_counter_before, "Server counter did not increase after restore.");

//     println!("Test completed successfully.");
// }
