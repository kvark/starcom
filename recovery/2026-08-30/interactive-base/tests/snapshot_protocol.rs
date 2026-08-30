//! A barrier is a command reply, not an SSH read or a newline-shaped data chunk.
use starcom::control;

#[test]
fn output_after_last_reply_is_retained_at_every_network_split() {
    let wire = b"%begin 1 1 0\n%end 1 1 0\n%begin 1 2 1\nsnapshot\n%end 1 2 1\n%begin 1 3 1\n%end 1 3 1\n%output %1 \\033[2KZ\n";
    for split in 0..=wire.len() {
        let mut control = control::Control::default();
        let ids = [
            control.register_command().unwrap(),
            control.register_command().unwrap(),
        ];
        let mut completed = 0;
        let mut after_barrier = Vec::new();
        for chunk in [&wire[..split], &wire[split..]] {
            control
                .feed(chunk, |event| match event {
                    tmuxctl::Incoming::Reply { id, result } => {
                        assert_eq!(ids[completed], id);
                        assert!(result.is_ok());
                        completed += 1;
                    }
                    tmuxctl::Incoming::Notification(tmuxctl::Notification::Output {
                        bytes,
                        ..
                    }) => {
                        assert_eq!(completed, 2);
                        after_barrier.extend(bytes);
                    }
                    _ => {}
                })
                .unwrap();
        }
        assert_eq!(after_barrier, b"\x1b[2KZ");
        assert_eq!(control.pending_commands(), 0);
    }
}
