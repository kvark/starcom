use std::process;

#[test]
fn desktop_and_replay_options_cannot_be_mixed() {
    for arguments in [
        vec!["--demo", "--replay", "not-read"],
        vec!["--size", "80x24"],
        vec!["--demo", "--demo"],
    ] {
        let output = process::Command::new(env!("CARGO_BIN_EXE_starcom"))
            .args(arguments)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!output.stderr.contains(&0x1b));
    }
}

#[test]
fn snapshot_export_requires_explicit_demo_data() {
    let output = process::Command::new(env!("CARGO_BIN_EXE_starcom"))
        .args(["--snapshot", "must-not-be-written.png"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let message = String::from_utf8(output.stderr).unwrap();
    #[cfg(feature = "gui")]
    assert!(message.contains("requires --demo"));
    #[cfg(not(feature = "gui"))]
    assert!(message.contains("requires the gui feature"));
}
