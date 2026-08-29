#![cfg(all(feature = "ssh", target_os = "linux"))]
//! Only scripts/test-ssh.sh supplies this disposable, loopback-only fixture.
use starcom::{core, inspect, ssh};
use std::{env, fs, io, path, process, thread, time};

fn root() -> path::PathBuf {
    path::PathBuf::from(env::var_os("STARCOM_TEST_DIR").expect("run scripts/test-ssh.sh"))
}

fn options() -> ssh::Options {
    ssh::Options {
        host: "127.0.0.1".to_owned(),
        port: fs::read_to_string(root().join("port"))
            .unwrap()
            .trim()
            .parse()
            .unwrap(),
        user: env::var("STARCOM_TEST_USER").unwrap(),
        known_hosts: root().join("known_hosts"),
        authentication: ssh::Authentication::Identity(root().join("id_ed25519")),
        timeout: time::Duration::from_secs(5),
    }
}

fn tmux(args: &[&str]) -> String {
    let result = process::Command::new("timeout")
        .args(["5s", "tmux", "-S"])
        .arg(root().join("tmux.sock"))
        .args(args)
        .output()
        .unwrap();
    assert!(result.status.success(), "tmux: {:?}", result.stderr);
    String::from_utf8(result.stdout).unwrap()
}

#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn inspect_existing_primary_and_alternate_screens_without_mutation() {
    let before = tmux(&[
        "list-panes",
        "-s",
        "-t",
        "starcom",
        "-F",
        "#{pane_id} #{pane_width} #{pane_height} #{pane_pid}",
    ]);
    let environment = tmux(&["show-environment", "-t", "starcom", "STARCOM_TEST_ENV"]);
    let session = core::SessionName::new("starcom").unwrap();
    let socket = root().join("tmux.sock");
    let mut inspector = inspect::Inspector::attach(&options(), &session, socket.to_str()).unwrap();
    let observed = inspector.observe(20).unwrap();
    assert_eq!(observed.captures.len(), 2);
    assert!(
        observed
            .captures
            .iter()
            .any(|capture| capture.pane.alternate_screen)
    );
    assert!(observed.fingerprint.starts_with("SHA256:"));
    let rows: Vec<_> = observed
        .captures
        .iter()
        .flat_map(|capture| &capture.escaped_rows)
        .collect();
    assert!(rows.iter().any(|row| row.contains("STARCOM_PRIMARY_READY")));
    assert!(
        rows.iter()
            .any(|row| row.contains("STARCOM_ALTERNATE_READY"))
    );
    drop(inspector);
    assert_eq!(
        before,
        tmux(&[
            "list-panes",
            "-s",
            "-t",
            "starcom",
            "-F",
            "#{pane_id} #{pane_width} #{pane_height} #{pane_pid}"
        ])
    );
    assert_eq!(
        environment,
        tmux(&["show-environment", "-t", "starcom", "STARCOM_TEST_ENV"])
    );
    let deadline = time::Instant::now() + time::Duration::from_secs(5);
    while !tmux(&["list-clients", "-F", "#{client_pid}"])
        .trim()
        .is_empty()
    {
        assert!(
            time::Instant::now() < deadline,
            "inspection client did not detach"
        );
        thread::sleep(time::Duration::from_millis(20));
    }
    tmux(&["has-session", "-t", "=starcom"]);
}

#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn trust_failures_precede_authentication_and_do_not_rewrite_known_hosts() {
    for (name, kind) in [
        ("known_hosts.empty", ssh::Kind::UnknownHostKey),
        ("known_hosts.bad", ssh::Kind::ChangedHostKey),
        ("known_hosts.revoked", ssh::Kind::Configuration),
    ] {
        let mut options = options();
        options.known_hosts = root().join(name);
        options.authentication = ssh::Authentication::Identity(root().join("DOES_NOT_EXIST"));
        let before = fs::read(&options.known_hosts).unwrap();
        let error = ssh::Connection::connect(&options)
            .err()
            .expect("unsafe connection succeeded");
        assert_eq!(error.kind, kind, "{error}");
        assert_eq!(before, fs::read(&options.known_hosts).unwrap());
    }
}

#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn hashed_known_hosts_and_explicit_agent_authentication_work() {
    let mut options = options();
    options.known_hosts = root().join("known_hosts.hashed");
    drop(ssh::Connection::connect(&options).unwrap());
    options.authentication = ssh::Authentication::Agent;
    drop(ssh::Connection::connect(&options).unwrap());
}

#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn missing_sessions_and_servers_are_not_created() {
    let socket = root().join("tmux.sock");
    let missing = core::SessionName::new("missing-starcom-session").unwrap();
    let result = inspect::Inspector::attach(&options(), &missing, socket.to_str())
        .and_then(|mut inspector| inspector.observe(0));
    assert!(result.is_err());
    let absent_socket = root().join("absent.sock");
    let result = inspect::Inspector::attach(&options(), &missing, absent_socket.to_str())
        .and_then(|mut inspector| inspector.observe(0));
    assert!(result.is_err());
    assert!(
        !absent_socket.exists(),
        "inspection created a new tmux server"
    );
    assert_eq!(
        tmux(&["list-sessions", "-F", "#{session_name}"]).trim(),
        "starcom"
    );
}

#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn stdout_and_stderr_remain_separate_without_a_pty() {
    let mut channel = ssh::Connection::connect(&options())
        .unwrap()
        .exec("printf output; printf diagnostic >&2; test ! -t 0")
        .unwrap();
    let deadline = time::Instant::now() + time::Duration::from_secs(5);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut buffer = [0; 1024];
    loop {
        assert!(time::Instant::now() < deadline);
        let mut progress = false;
        match channel.read_stderr(&mut buffer) {
            Ok(count) => {
                stderr.extend_from_slice(&buffer[..count]);
                progress |= count != 0;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("stderr: {error}"),
        }
        match io::Read::read(&mut channel, &mut buffer) {
            Ok(count) => {
                stdout.extend_from_slice(&buffer[..count]);
                progress |= count != 0;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("stdout: {error}"),
        }
        if channel.eof() {
            break;
        }
        if !progress {
            channel.wait(deadline).unwrap();
        }
    }
    assert_eq!(stdout, b"output");
    assert_eq!(stderr, b"diagnostic");
}

#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn stalled_channel_has_a_deadline() {
    let mut channel = ssh::Connection::connect(&options())
        .unwrap()
        .exec("exec sleep 2")
        .unwrap();
    let deadline = time::Instant::now() + time::Duration::from_millis(100);
    let mut buffer = [0; 16];
    loop {
        match io::Read::read(&mut channel, &mut buffer) {
            Ok(0) => panic!("sleep exited before timeout"),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("read: {error}"),
        }
        if let Err(error) = channel.wait(deadline) {
            assert_eq!(error.kind, ssh::Kind::Timeout);
            break;
        }
    }
}

#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn inspection_cli_exercises_the_real_embedded_transport() {
    let options = options();
    let result = process::Command::new(env!("CARGO_BIN_EXE_starcom-inspect"))
        .args([
            "--host",
            "127.0.0.1",
            "--user",
            &options.user,
            "--session",
            "starcom",
            "--port",
            &options.port.to_string(),
        ])
        .arg("--known-hosts")
        .arg(&options.known_hosts)
        .arg("--identity")
        .arg(root().join("id_ed25519"))
        .arg("--socket")
        .arg(root().join("tmux.sock"))
        .args(["--history", "0", "--timeout", "5"])
        .output()
        .unwrap();
    assert!(result.status.success(), "CLI stderr: {:?}", result.stderr);
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert!(stdout.contains("STARCOM_PRIMARY_READY"));
    assert!(stdout.contains("STARCOM_ALTERNATE_READY"));
    assert!(stdout.contains("Read-only observations"));
    assert!(!stdout.contains('\x1b'));
}
