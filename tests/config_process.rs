use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use tempfile::TempDir;

const CONFIG_NAMES: [&str; 9] = [
    "AUTOINDEX_DIRECTORY",
    "AUTOINDEX_BIND",
    "AUTOINDEX_PORT",
    "AUTOINDEX_README",
    "AUTOINDEX_INDEX_FILES",
    "AUTOINDEX_PAGE_SIZE",
    "AUTOINDEX_TIMEZONE",
    "AUTOINDEX_LOG_LEVEL",
    "AUTOINDEX_ALLOW_SENSITIVE_PATHS",
];
static PROCESS_TEST_LOCK: Mutex<()> = Mutex::new(());

fn lock_process_tests() -> MutexGuard<'static, ()> {
    PROCESS_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct ServerProcess(Child);

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn fixture_workspace() -> (TempDir, std::path::PathBuf) {
    let temporary = TempDir::new().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    (temporary, workspace)
}

fn create_source(workspace: &Path, name: &str, marker: &str) {
    let directory = workspace.join(name);
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join(format!("{marker}.txt")), marker).unwrap();
    std::fs::write(directory.join("README.md"), format!("# {marker} README\n")).unwrap();
}

fn start_server(
    workspace: &Path,
    arguments: &[OsString],
    environment: &[(&str, String)],
    port: u16,
) -> ServerProcess {
    let mut command = Command::new(env!("CARGO_BIN_EXE_autoindex-rs"));
    command
        .current_dir(workspace)
        .args(arguments)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for name in CONFIG_NAMES {
        command.env_remove(name);
    }
    for (name, value) in environment {
        command.env(name, value);
    }
    let process = ServerProcess(command.spawn().unwrap());
    for _ in 0..100 {
        if fetch(port).is_ok() {
            return process;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("server did not listen on 127.0.0.1:{port}");
}

fn fetch(port: u16) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

#[test]
fn dotenv_is_loaded_from_the_startup_working_directory() {
    let _guard = lock_process_tests();
    let (_temporary, workspace) = fixture_workspace();
    create_source(&workspace, "dotenv-source", "dotenv-marker");
    let port = free_port();
    std::fs::write(
        workspace.join(".env"),
        format!(
            "AUTOINDEX_DIRECTORY=dotenv-source\nAUTOINDEX_BIND=127.0.0.1\nAUTOINDEX_PORT={port}\nAUTOINDEX_README=false\nAUTOINDEX_INDEX_FILES=\n"
        ),
    )
    .unwrap();

    let _server = start_server(&workspace, &[], &[], port);
    let response = fetch(port).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.contains("dotenv-marker.txt"));
    assert!(!response.contains("dotenv-marker README"));
}

#[test]
fn process_environment_overrides_dotenv() {
    let _guard = lock_process_tests();
    let (_temporary, workspace) = fixture_workspace();
    create_source(&workspace, "dotenv-source", "dotenv-marker");
    create_source(&workspace, "environment-source", "environment-marker");
    let dotenv_port = free_port();
    let environment_port = free_port();
    std::fs::write(
        workspace.join(".env"),
        format!(
            "AUTOINDEX_DIRECTORY=dotenv-source\nAUTOINDEX_BIND=127.0.0.1\nAUTOINDEX_PORT={dotenv_port}\n"
        ),
    )
    .unwrap();
    let environment = [
        ("AUTOINDEX_DIRECTORY", "environment-source".to_string()),
        ("AUTOINDEX_PORT", environment_port.to_string()),
    ];

    let _server = start_server(&workspace, &[], &environment, environment_port);
    let response = fetch(environment_port).unwrap();
    assert!(response.contains("environment-marker.txt"));
    assert!(!response.contains("dotenv-marker.txt"));
}

#[test]
fn command_line_overrides_process_environment() {
    let _guard = lock_process_tests();
    let (_temporary, workspace) = fixture_workspace();
    create_source(&workspace, "environment-source", "environment-marker");
    create_source(&workspace, "cli-source", "cli-marker");
    let environment_port = free_port();
    let cli_port = free_port();
    std::fs::write(
        workspace.join(".env"),
        "AUTOINDEX_BIND=127.0.0.1\nAUTOINDEX_README=false\n",
    )
    .unwrap();
    let environment = [
        ("AUTOINDEX_DIRECTORY", "environment-source".to_string()),
        ("AUTOINDEX_PORT", environment_port.to_string()),
    ];
    let arguments = [
        OsString::from("cli-source"),
        OsString::from("--port"),
        OsString::from(cli_port.to_string()),
        OsString::from("--readme"),
    ];

    let _server = start_server(&workspace, &arguments, &environment, cli_port);
    let response = fetch(cli_port).unwrap();
    assert!(response.contains("cli-marker.txt"));
    assert!(response.contains("cli-marker README"));
    assert!(!response.contains("environment-marker.txt"));
}

#[test]
fn malformed_dotenv_fails_before_server_start() {
    let _guard = lock_process_tests();
    let (_temporary, workspace) = fixture_workspace();
    std::fs::write(workspace.join(".env"), "AUTOINDEX_PORT='unterminated\n").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_autoindex-rs"));
    command.current_dir(&workspace);
    for name in CONFIG_NAMES {
        command.env_remove(name);
    }
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("autoindex-rs:"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
