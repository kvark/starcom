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
