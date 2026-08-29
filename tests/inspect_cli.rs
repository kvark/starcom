#![cfg(feature = "ssh")]
use std::process;

#[test]
fn inspection_help_is_honest_about_scope() {
    let output = process::Command::new(env!("CARGO_BIN_EXE_starcom-inspect"))
        .arg("--help").output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("read-only"));
    assert!(text.contains("does not read ~/.ssh/config"));
    assert!(text.contains("NOT an atomic/interactive session"));
}

#[test]
fn invalid_options_fail_before_networking() {
    for args in [vec!["--nonsense"], vec!["--host"], vec!["--host", "x", "--host", "y"],
        vec!["--host", "x", "--user", "u", "--session", "work", "--known-hosts", "missing", "--agent", "--identity", "missing"]]
    {
        let output = process::Command::new(env!("CARGO_BIN_EXE_starcom-inspect"))
            .args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(!output.stderr.contains(&0x1b));
    }
}
