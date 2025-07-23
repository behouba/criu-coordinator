use std::{
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

pub const CRIU_COORDINATOR_PATH: &str = "target/debug/criu-coordinator";

pub fn pick_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn server_ready(addr: &str, retries: u32) -> bool {
    for _ in 0..retries {
        if TcpStream::connect(addr).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

pub fn spawn_server(address: &str, port: u16) -> Child {
    let mut cmd = Command::new("target/debug/criu-coordinator");
    cmd.arg("server")
        .arg("--address")
        .arg(address)
        .arg("--port")
        .arg(port.to_string())
        .arg("--max-retries")
        .arg("5");
    println!("Spawning server: {:?}", cmd);
    cmd.spawn().expect("Failed to spawn server")
}
