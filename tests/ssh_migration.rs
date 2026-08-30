#![cfg(all(feature = "ssh", target_os = "linux"))]
//! Rust-native SSH migration and desktop-worker acceptance against an isolated host.

use starcom::ssh;
use std::{env, fs, io, path, time};

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

#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn rustcrypto_identity_algorithms_and_rekeyed_stream_work() {
    for key in ["id_ed25519", "id_rsa", "id_ecdsa"] {
        let mut options = options();
        options.authentication = ssh::Authentication::Identity(root().join(key));
        let mut channel = ssh::Connection::connect(&options)
            .unwrap()
            .exec("python3 -c 'import sys; sys.stdout.write(\"0123456789abcdef\" * 8192)' ")
            .unwrap();
        let deadline = time::Instant::now() + time::Duration::from_secs(10);
        let mut received = Vec::new();
        let mut buffer = [0; 8192];
        while !channel.eof() {
            assert!(time::Instant::now() < deadline, "rekeyed stream stalled");
            match io::Read::read(&mut channel, &mut buffer) {
                Ok(count) => received.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    channel.wait(deadline).unwrap()
                }
                Err(error) => panic!("stream: {error}"),
            }
        }
        assert_eq!(received, b"0123456789abcdef".repeat(8192), "{key}");
    }
}

#[cfg(feature = "gui")]
#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn desktop_worker_publishes_live_view_and_rejects_cancelled_requests() {
    use starcom::{core, desktop, session};
    use std::{sync, thread};

    let client = desktop::Client::new(sync::Arc::new(|| {})).unwrap();
    let connection = desktop::Connection {
        options: options(),
        session: core::SessionName::new("starcom").unwrap(),
        socket: Some(root().join("tmux.sock").to_str().unwrap().to_owned()),
        history: 20,
        access: session::Access::Interactive,
        reconnect: false,
    };
    client.connect(connection.clone()).unwrap();
    let deadline = time::Instant::now() + time::Duration::from_secs(10);
    while client.phase() != desktop::Phase::Watching {
        assert!(
            time::Instant::now() < deadline,
            "worker did not attach: {:?}",
            client.phase()
        );
        thread::sleep(time::Duration::from_millis(10));
    }
    client.with_view(|view| {
        let view = view.unwrap();
        assert_eq!(view.panes().len(), 2);
        assert!(view.panes().values().any(|pane| {
            pane.terminal
                .screen_lines()
                .iter()
                .any(|row| row.contains("STARCOM_PRIMARY_READY"))
        }));
    });
    client.disconnect();
    assert_eq!(client.phase(), desktop::Phase::Disconnected);
    client.with_view(|view| assert!(view.is_some(), "disconnect discarded the last view"));
    client.connect(connection).unwrap();
    client.demo().unwrap();
    thread::sleep(time::Duration::from_millis(300));
    assert_eq!(
        client.phase(),
        desktop::Phase::Demo,
        "stale connection replaced demo"
    );
    client.with_view(|view| assert_eq!(view.unwrap().panes().len(), 3));
}

#[cfg(feature = "gui")]
#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn desktop_worker_delivers_input_paste_and_resize_once() {
    use starcom::{core, desktop, input, session};
    use std::{os::unix::fs::PermissionsExt as _, process::Command, sync, thread};

    let test_root = root();
    let socket = test_root.join("tmux.sock");
    let reader = test_root.join("input-reader.sh");
    fs::write(
        &reader,
        r#"#!/bin/sh
stty -echo
printf '\033[2J\033[HINPUT_READY\r\n'
IFS= read -r first
IFS= read -r second
printf 'INPUT_RESULT:<%s>|<%s>\r\n' "$first" "$second"
exec sleep 600
"#,
    )
    .unwrap();
    let mut permissions = fs::metadata(&reader).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&reader, permissions).unwrap();

    let tmux = || {
        let mut command = Command::new("tmux");
        command.arg("-S").arg(&socket);
        command
    };
    let _ = tmux()
        .args(["kill-session", "-t", "starcom-input"])
        .status();
    assert!(
        tmux()
            .args([
                "new-session",
                "-d",
                "-s",
                "starcom-input",
                "-x",
                "100",
                "-y",
                "30",
            ])
            .arg(&reader)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        tmux()
            .args([
                "split-window",
                "-h",
                "-t",
                "starcom-input",
                "exec sleep 600",
            ])
            .status()
            .unwrap()
            .success()
    );

    let ready_deadline = time::Instant::now() + time::Duration::from_secs(5);
    loop {
        let output = tmux()
            .args(["capture-pane", "-p", "-t", "starcom-input:0.0"])
            .output()
            .unwrap();
        assert!(output.status.success());
        if String::from_utf8_lossy(&output.stdout).contains("INPUT_READY") {
            break;
        }
        assert!(
            time::Instant::now() < ready_deadline,
            "input pane did not start"
        );
        thread::sleep(time::Duration::from_millis(20));
    }

    let pane_output = tmux()
        .args([
            "display-message",
            "-p",
            "-t",
            "starcom-input:0.0",
            "#{pane_id}",
        ])
        .output()
        .unwrap();
    assert!(pane_output.status.success());
    let pane = tmuxctl::PaneId(
        String::from_utf8(pane_output.stdout)
            .unwrap()
            .trim()
            .strip_prefix('%')
            .unwrap()
            .parse()
            .unwrap(),
    );

    let client = desktop::Client::new(sync::Arc::new(|| {})).unwrap();
    let connection = desktop::Connection {
        options: options(),
        session: core::SessionName::new("starcom-input").unwrap(),
        socket: Some(socket.to_str().unwrap().to_owned()),
        history: 50,
        access: session::Access::Interactive,
        reconnect: false,
    };
    client.connect(connection.clone()).unwrap();
    let attach_deadline = time::Instant::now() + time::Duration::from_secs(10);
    while client.phase() != desktop::Phase::Watching {
        assert!(
            time::Instant::now() < attach_deadline,
            "desktop worker did not attach"
        );
        thread::sleep(time::Duration::from_millis(10));
    }

    let target = client.target(pane).expect("interactive pane target");
    client
        .submit(target, input::Action::Bytes(b"hello".to_vec()))
        .unwrap();
    client
        .submit(
            target,
            input::Action::Key(input::Key::Enter, input::Modifiers::default()),
        )
        .unwrap();
    client
        .submit(
            target,
            input::Action::Paste(input::Paste::new("world\n").unwrap()),
        )
        .unwrap();

    let result_deadline = time::Instant::now() + time::Duration::from_secs(10);
    loop {
        let present = client.with_view(|view| {
            view.and_then(|view| view.panes().get(&pane))
                .is_some_and(|pane| {
                    pane.terminal
                        .screen_lines()
                        .iter()
                        .any(|line| line.contains("INPUT_RESULT:<hello>|<world>"))
                })
        });
        if present {
            break;
        }
        assert!(
            time::Instant::now() < result_deadline,
            "terminal input or paste was lost"
        );
        thread::sleep(time::Duration::from_millis(10));
    }

    let before = client.with_view(|view| view.unwrap().panes()[&pane].state.size.columns());
    let desired = before + 5;
    client.allow_remote_resize(true);
    let target = client.target(pane).expect("target before resize");
    client
        .submit(
            target,
            input::Action::Resize(input::Resize {
                axis: input::Axis::Columns,
                cells: desired,
            }),
        )
        .unwrap();
    let resize_deadline = time::Instant::now() + time::Duration::from_secs(10);
    loop {
        let resized = client.phase() == desktop::Phase::Watching
            && client.with_view(|view| {
                view.and_then(|view| view.panes().get(&pane))
                    .is_some_and(|pane| pane.state.size.columns() == desired)
            });
        if resized {
            break;
        }
        assert!(
            time::Instant::now() < resize_deadline,
            "remote pane was not resynchronized"
        );
        thread::sleep(time::Duration::from_millis(10));
    }

    let capture = tmux()
        .args(["capture-pane", "-p", "-S", "-50", "-t", "starcom-input:0.0"])
        .output()
        .unwrap();
    assert!(capture.status.success());
    assert_eq!(
        String::from_utf8_lossy(&capture.stdout)
            .matches("INPUT_RESULT:<hello>|<world>")
            .count(),
        1,
        "input or paste was duplicated"
    );

    let mut direct = session::Session::attach_with_access(
        &connection.options,
        &connection.session,
        connection.socket.as_deref(),
        connection.history,
        session::Access::Interactive,
    )
    .unwrap();
    assert!(
        tmux()
            .args([
                "set-window-option",
                "-t",
                "starcom-input",
                "synchronize-panes",
                "on",
            ])
            .status()
            .unwrap()
            .success()
    );
    let direct_pane = *direct.view().panes().keys().next().unwrap();
    let error = direct
        .interact(direct_pane, &input::Action::Bytes(b"blocked".to_vec()))
        .unwrap_err();
    assert!(error.to_string().contains("synchronize-panes"));

    client.disconnect();
    let _ = tmux()
        .args(["kill-session", "-t", "starcom-input"])
        .status();
}

/// M3: a transport drop must reattach on its own, publish freshly reconstructed
/// models under a new epoch, and never replay what was in flight when it dropped.
#[cfg(feature = "gui")]
#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn transport_loss_reattaches_without_replaying_input_or_creating_a_session() {
    use starcom::{core, desktop, input, session};
    use std::sync;

    let socket = root().join("tmux.sock");
    let session_name = "starcom-reconnect";
    let _ = tmux()
        .args(["kill-session", "-t", session_name])
        .status()
        .unwrap();
    assert!(
        tmux()
            .args([
                "new-session",
                "-d",
                "-s",
                session_name,
                "-x",
                "80",
                "-y",
                "24",
                "cat > /dev/null",
            ])
            .status()
            .unwrap()
            .success()
    );

    let client = desktop::Client::new(sync::Arc::new(|| {})).unwrap();
    client
        .connect(desktop::Connection {
            options: options(),
            session: core::SessionName::new(session_name).unwrap(),
            socket: Some(socket.to_str().unwrap().to_owned()),
            history: 20,
            access: session::Access::Interactive,
            reconnect: true,
        })
        .unwrap();
    wait_until(20, "worker did not attach", || {
        client.phase() == desktop::Phase::Watching
    });

    let pane = client.with_view(|view| *view.unwrap().panes().keys().next().unwrap());
    let first_generation = client.generation();
    let first_session = client.with_view(|view| view.unwrap().session);

    // Something identifiable on screen, delivered exactly once.
    let target = client.target(pane).expect("live target");
    client
        .submit(target, input::Action::Bytes(b"BEFORE-DROP\r".to_vec()))
        .unwrap();
    wait_until(20, "input never reached the pane", || {
        pane_text(session_name).contains("BEFORE-DROP")
    });

    // Kill the tmux client process behind the control channel. The channel dies
    // with no %exit, which is what an abrupt transport loss looks like.
    let clients = tmux()
        .args(["list-clients", "-t", session_name, "-F", "#{client_pid}"])
        .output()
        .unwrap();
    let control_pid: i32 = String::from_utf8_lossy(&clients.stdout)
        .split_whitespace()
        .next()
        .expect("the control client must be attached")
        .parse()
        .unwrap();
    assert!(
        std::process::Command::new("kill")
            .args(["-9", &control_pid.to_string()])
            .status()
            .unwrap()
            .success()
    );

    // While offline the last view stays readable, but nothing may be queued.
    wait_until(20, "worker did not notice the drop", || {
        matches!(
            client.phase(),
            desktop::Phase::Reconnecting | desktop::Phase::Connecting
        )
    });
    client.with_view(|view| assert!(view.is_some(), "the last view was discarded"));
    assert!(
        client.target(pane).is_none(),
        "input tokens must not be issued while disconnected"
    );
    assert!(
        client
            .submit(target, input::Action::Bytes(b"NEVER-SENT\r".to_vec()))
            .is_err(),
        "input must not be queued for replay after a drop"
    );

    // It comes back by itself, with rebuilt models under a new epoch.
    wait_until(60, "worker did not reattach", || {
        client.phase() == desktop::Phase::Watching
    });
    assert!(
        client.generation() > first_generation,
        "reattachment must publish freshly reconstructed models"
    );
    assert_eq!(
        client.with_view(|view| view.unwrap().session),
        first_session,
        "the same tmux session must be reattached, never a replacement"
    );
    assert!(
        client.retry().is_none(),
        "retry state outlived the recovery"
    );
    let recovered = client.target(pane).expect("input is live again");
    assert_ne!(
        recovered, target,
        "a token from the lost attachment must not compare equal to a new one"
    );

    // Nothing was replayed, and no second session was invented.
    let text = pane_text(session_name);
    assert_eq!(
        text.matches("BEFORE-DROP").count(),
        1,
        "output was duplicated across the reconnect"
    );
    assert!(
        !text.contains("NEVER-SENT"),
        "input rejected while offline was delivered anyway"
    );
    let sessions = tmux()
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&sessions.stdout)
            .lines()
            .filter(|name| name.starts_with(session_name))
            .count(),
        1,
        "reconnection created a replacement session"
    );

    client.disconnect();
    let _ = tmux().args(["kill-session", "-t", session_name]).status();
}

/// A destroyed session is not a transport fault: it must stop and say so rather
/// than reattaching to whatever now answers to that name.
#[cfg(feature = "gui")]
#[test]
#[ignore = "requires the isolated SSH/tmux fixture"]
fn a_destroyed_session_stops_instead_of_reattaching() {
    use starcom::{core, desktop, session};
    use std::{sync, thread};

    let session_name = "starcom-destroyed";
    let _ = tmux()
        .args(["kill-session", "-t", session_name])
        .status()
        .unwrap();
    assert!(
        tmux()
            .args([
                "new-session",
                "-d",
                "-s",
                session_name,
                "-x",
                "80",
                "-y",
                "24",
                "cat > /dev/null",
            ])
            .status()
            .unwrap()
            .success()
    );
    let client = desktop::Client::new(sync::Arc::new(|| {})).unwrap();
    client
        .connect(desktop::Connection {
            options: options(),
            session: core::SessionName::new(session_name).unwrap(),
            socket: Some(root().join("tmux.sock").to_str().unwrap().to_owned()),
            history: 20,
            access: session::Access::ReadOnly,
            reconnect: true,
        })
        .unwrap();
    wait_until(20, "worker did not attach", || {
        client.phase() == desktop::Phase::Watching
    });
    assert!(
        tmux()
            .args(["kill-session", "-t", session_name])
            .status()
            .unwrap()
            .success()
    );
    wait_until(20, "worker kept watching a destroyed session", || {
        matches!(
            client.phase(),
            desktop::Phase::Disconnected | desktop::Phase::Failed
        )
    });
    // Give any (incorrect) retry schedule time to fire before asserting.
    thread::sleep(time::Duration::from_secs(2));
    assert!(
        matches!(
            client.phase(),
            desktop::Phase::Disconnected | desktop::Phase::Failed
        ),
        "a destroyed session must not be reattached automatically: {:?}",
        client.phase()
    );
    assert!(client.retry().is_none(), "a destroyed session was retried");
    client.with_view(|view| assert!(view.is_some(), "the last view was discarded"));
}

/// The disposable fixture socket. Never the contributor's default tmux server.
fn tmux() -> std::process::Command {
    let mut command = std::process::Command::new("tmux");
    command.arg("-S").arg(root().join("tmux.sock"));
    command
}

fn pane_text(session: &str) -> String {
    let capture = tmux()
        .args(["capture-pane", "-p", "-S", "-50", "-t", session])
        .output()
        .unwrap();
    assert!(capture.status.success());
    String::from_utf8_lossy(&capture.stdout).into_owned()
}

fn wait_until(seconds: u64, message: &str, mut ready: impl FnMut() -> bool) {
    let deadline = time::Instant::now() + time::Duration::from_secs(seconds);
    while !ready() {
        assert!(time::Instant::now() < deadline, "{message}");
        std::thread::sleep(time::Duration::from_millis(20));
    }
}
