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

#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn live_models_restore_existing_primary_and_alternate_screens() {
    let name = core::SessionName::new("starcom").unwrap();
    let socket = root().join("tmux.sock");
    let session =
        starcom::session::Session::attach(&options(), &name, socket.to_str(), 20).unwrap();
    assert_eq!(session.view().panes().len(), 2);
    let rows: Vec<_> = session
        .view()
        .panes()
        .values()
        .flat_map(|pane| pane.terminal.screen_lines())
        .collect();
    assert!(
        rows.iter()
            .any(|line| line.contains("STARCOM_PRIMARY_READY"))
    );
    assert!(
        rows.iter()
            .any(|line| line.contains("STARCOM_ALTERNATE_READY"))
    );
    assert!(
        session
            .view()
            .panes()
            .values()
            .any(|pane| pane.terminal.is_alternate_screen())
    );
}

struct LiveFixture {
    name: String,
}

impl Drop for LiveFixture {
    fn drop(&mut self) {
        let _ = process::Command::new("timeout")
            .args(["5s", "tmux", "-S"])
            .arg(root().join("tmux.sock"))
            .args(["kill-session", "-t", &format!("={}", self.name)])
            .output();
    }
}

fn wait_for(mut predicate: impl FnMut() -> bool) {
    let deadline = time::Instant::now() + time::Duration::from_secs(5);
    while !predicate() {
        assert!(
            time::Instant::now() < deadline,
            "fixture condition timed out"
        );
        thread::sleep(time::Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn snapshot_pending_bytes_resume_once_and_resize_rebuilds_models() {
    let fixture = LiveFixture {
        name: "starcom-sync-fragment".to_owned(),
    };
    let script = root().join("pending.py");
    let advance = root().join("advance");
    let _ = fs::remove_file(&advance);
    fs::write(
        &script,
        concat!(
            "import pathlib, sys, time\n",
            "root = pathlib.Path(__file__).parent\n",
            "sys.stdout.write('\\x1b[2J\\x1b[HBASE\\x1b[2')\n",
            "sys.stdout.flush()\n",
            "while not (root / 'advance').exists(): time.sleep(0.01)\n",
            "sys.stdout.write('K\\rZ')\n",
            "sys.stdout.flush()\n",
            "time.sleep(60)\n",
        ),
    )
    .unwrap();
    tmux(&[
        "new-session",
        "-d",
        "-s",
        &fixture.name,
        "-x",
        "40",
        "-y",
        "8",
        &format!("exec python3 '{}'", script.display()),
    ]);
    let pane = tmux(&["list-panes", "-t", &fixture.name, "-F", "#{pane_id}"])
        .trim()
        .to_owned();
    let pid = tmux(&["display-message", "-p", "-t", &pane, "#{pane_pid}"]);
    wait_for(|| tmux(&["capture-pane", "-p", "-P", "-C", "-t", &pane]).contains("\\033[2"));
    let name = core::SessionName::new(&fixture.name).unwrap();
    let socket = root().join("tmux.sock");
    let mut session =
        starcom::session::Session::attach(&options(), &name, socket.to_str(), 20).unwrap();
    let id = *session.view().panes().keys().next().unwrap();
    assert_eq!(
        session.view().panes()[&id].terminal.screen_lines()[0],
        "BASE"
    );
    fs::write(advance, b"next").unwrap();
    let deadline = time::Instant::now() + time::Duration::from_secs(5);
    while session.view().panes()[&id].terminal.screen_lines()[0] != "Z" {
        assert!(
            time::Instant::now() < deadline,
            "pending escape was lost or duplicated"
        );
        session.poll(deadline).unwrap();
        assert_eq!(session.view().status(), starcom::snapshot::Status::Watching);
    }
    assert_eq!(
        tmux(&["capture-pane", "-p", "-t", &pane]).lines().next(),
        Some("Z")
    );
    tmux(&["resize-window", "-t", &fixture.name, "-x", "60", "-y", "10"]);
    let deadline = time::Instant::now() + time::Duration::from_secs(5);
    while session.view().status() == starcom::snapshot::Status::Watching {
        assert!(
            time::Instant::now() < deadline,
            "resize notification not observed"
        );
        session.poll(deadline).unwrap();
    }
    assert_eq!(
        session.view().status(),
        starcom::snapshot::Status::NeedsResync
    );
    session.synchronize().unwrap();
    assert_eq!(
        session.view().panes()[&id].terminal.size(),
        core::Size::new(60, 10).unwrap()
    );
    assert_eq!(session.view().panes()[&id].terminal.screen_lines()[0], "Z");
    assert_eq!(
        pid,
        tmux(&["display-message", "-p", "-t", &pane, "#{pane_pid}"])
    );
    tmux(&["detach-client", "-s", &fixture.name]);
    let deadline = time::Instant::now() + time::Duration::from_secs(5);
    while session.view().status() != starcom::snapshot::Status::Disconnected {
        assert!(time::Instant::now() < deadline, "disconnect not observed");
        let _ = session.poll(deadline);
    }
    assert_eq!(session.view().panes()[&id].terminal.screen_lines()[0], "Z");
    assert_eq!(
        pid,
        tmux(&["display-message", "-p", "-t", &pane, "#{pane_pid}"])
    );
}

#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn watch_cli_uses_embedded_transport_and_escapes_its_final_screens() {
    let options = options();
    let result = process::Command::new("timeout")
        .arg("15s")
        .arg(env!("CARGO_BIN_EXE_starcom-inspect"))
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
        .args(["--history", "20", "--timeout", "5", "--watch", "1"])
        .output()
        .unwrap();
    assert!(result.status.success(), "watch stderr: {:?}", result.stderr);
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert!(stdout.contains("STARCOM_PRIMARY_READY"));
    assert!(stdout.contains("STARCOM_ALTERNATE_READY"));
    assert!(stdout.contains("Read-only live models"));
    assert!(!stdout.contains('\x1b'));
}

#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn continuous_output_crosses_snapshot_boundary_without_gaps_or_duplicates() {
    let fixture = LiveFixture {
        name: "starcom-sync-stream".to_owned(),
    };
    let script = root().join("continuous.py");
    let ready = root().join("continuous-ready");
    let _ = fs::remove_file(&ready);
    fs::write(
        &script,
        concat!(
            "import pathlib, sys, time\n",
            "root = pathlib.Path(__file__).parent\n",
            "sys.stdout.write('\\x1b[2J\\x1b[H')\n",
            "for i in range(200):\n",
            "    sys.stdout.write(f'record-{i:04d}\\r\\n')\n",
            "    sys.stdout.flush()\n",
            "    if i == 20: (root / 'continuous-ready').write_text('ready')\n",
            "    time.sleep(0.003)\n",
            "sys.stdout.write('END\\r\\n')\n",
            "sys.stdout.flush()\n",
            "time.sleep(60)\n",
        ),
    )
    .unwrap();
    tmux(&[
        "new-session",
        "-d",
        "-s",
        &fixture.name,
        "-x",
        "40",
        "-y",
        "8",
        &format!("exec python3 '{}'", script.display()),
    ]);
    wait_for(|| ready.exists());
    let name = core::SessionName::new(&fixture.name).unwrap();
    let socket = root().join("tmux.sock");
    let mut session =
        starcom::session::Session::attach(&options(), &name, socket.to_str(), 1000).unwrap();
    let id = *session.view().panes().keys().next().unwrap();
    let deadline = time::Instant::now() + time::Duration::from_secs(5);
    while !session.view().panes()[&id]
        .terminal
        .screen_lines()
        .iter()
        .any(|line| line == "END")
    {
        assert!(
            time::Instant::now() < deadline,
            "live output did not finish"
        );
        session.poll(deadline).unwrap();
        assert_eq!(session.view().status(), starcom::snapshot::Status::Watching);
    }
    let terminal = &session.view().panes()[&id].terminal;
    let history = alacritty_terminal::grid::Dimensions::history_size(terminal.model().grid());
    let mut records = Vec::new();
    for row in -(history as i32)..terminal.size().rows() as i32 {
        let text = terminal.model().bounds_to_string(
            alacritty_terminal::index::Point::new(
                alacritty_terminal::index::Line(row),
                alacritty_terminal::index::Column(0),
            ),
            alacritty_terminal::index::Point::new(
                alacritty_terminal::index::Line(row),
                alacritty_terminal::index::Column(terminal.size().columns() - 1),
            ),
        );
        let text = text.trim();
        if text.starts_with("record-") {
            records.push(text.to_owned());
        }
    }
    let expected: Vec<_> = (0..200).map(|index| format!("record-{index:04}")).collect();
    assert_eq!(
        records, expected,
        "snapshot/live cut lost or duplicated records"
    );
}

#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn snapshot_refuses_yielding_user_hooks_without_removing_them() {
    let fixture = LiveFixture {
        name: "starcom-sync-hooks".to_owned(),
    };
    let marker = root().join("hook-fired");
    let _ = fs::remove_file(&marker);
    tmux(&[
        "new-session",
        "-d",
        "-s",
        &fixture.name,
        "-x",
        "40",
        "-y",
        "8",
        "exec sleep 60",
    ]);
    tmux(&[
        "set-hook",
        "-t",
        &fixture.name,
        "after-capture-pane",
        &format!("run-shell 'touch {}; sleep 1'", marker.display()),
    ]);
    let before = tmux(&["show-hooks", "-t", &fixture.name]);
    let name = core::SessionName::new(&fixture.name).unwrap();
    let socket = root().join("tmux.sock");
    let result = starcom::session::Session::attach(&options(), &name, socket.to_str(), 20);
    let error = match result {
        Ok(_) => panic!("unsafe capture hook was accepted"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("after-capture-pane"),
        "unexpected error: {error:#}"
    );
    assert_eq!(before, tmux(&["show-hooks", "-t", &fixture.name]));
    assert!(!marker.exists(), "capture ran before rejecting the hook");
}
