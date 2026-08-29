//! Connection policy without sockets, clocks, sleeps, or an offline input queue.
//! Retry scheduling and terminal reconstruction belong to later milestones.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Epoch(u64);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum State {
    #[default]
    Disconnected,
    Connecting,
    Restoring,
    Live,
    Backoff,
    NeedsAttention(Failure),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    Transport,
    Authentication,
    HostKey,
    MissingSession,
    Protocol,
}

#[derive(Debug)]
pub struct Connection {
    epoch: Epoch,
    state: State,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            epoch: Epoch(0),
            state: State::Disconnected,
        }
    }
}

impl Connection {
    /// Explicitly start or supersede an attempt. The transport owner must close
    /// the old channel and fail its pending commands before replacing it.
    pub fn begin_connect(&mut self) -> Epoch {
        self.epoch = Epoch(
            self.epoch
                .0
                .checked_add(1)
                .expect("connection epoch exhausted"),
        );
        self.state = State::Connecting;
        self.epoch
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn transport_ready(&mut self, epoch: Epoch) -> bool {
        if epoch != self.epoch || self.state != State::Connecting {
            return false;
        }
        self.state = State::Restoring;
        true
    }

    /// Call only after a coherent topology/screen/mode restoration transaction.
    /// An SSH handshake alone must never enable input.
    pub fn restored(&mut self, epoch: Epoch) -> bool {
        if epoch != self.epoch || self.state != State::Restoring {
            return false;
        }
        self.state = State::Live;
        true
    }

    pub fn fail(&mut self, epoch: Epoch, failure: Failure) -> bool {
        if epoch != self.epoch
            || !matches!(
                self.state,
                State::Connecting | State::Restoring | State::Live
            )
        {
            return false;
        }
        self.state = match failure {
            Failure::Transport => State::Backoff,
            _ => State::NeedsAttention(failure),
        };
        true
    }

    pub fn disconnect(&mut self) {
        self.state = State::Disconnected;
    }

    pub fn accepts_output(&self, epoch: Epoch) -> bool {
        epoch == self.epoch && matches!(self.state, State::Restoring | State::Live)
    }

    pub fn accepts_input(&self, epoch: Epoch) -> bool {
        epoch == self.epoch && self.state == State::Live
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_does_not_enable_input() {
        let mut connection = Connection::default();
        let epoch = connection.begin_connect();
        assert!(!connection.accepts_input(epoch));
        assert!(!connection.restored(epoch));
        assert!(connection.transport_ready(epoch));
        assert!(connection.accepts_output(epoch));
        assert!(!connection.accepts_input(epoch));
        assert!(connection.restored(epoch));
        assert!(connection.accepts_input(epoch));
    }

    #[test]
    fn old_callbacks_cannot_modify_a_new_connection() {
        let mut connection = Connection::default();
        let old = connection.begin_connect();
        let current = connection.begin_connect();
        assert!(!connection.transport_ready(old));
        assert!(!connection.fail(old, Failure::HostKey));
        assert!(!connection.accepts_output(old));
        assert!(connection.transport_ready(current));
        assert!(connection.restored(current));
        assert!(!connection.accepts_input(old));
        assert!(connection.accepts_input(current));
    }

    #[test]
    fn disconnect_disables_input_and_rejects_late_success() {
        let mut connection = Connection::default();
        let epoch = connection.begin_connect();
        connection.transport_ready(epoch);
        connection.disconnect();
        assert!(!connection.restored(epoch));
        assert!(!connection.accepts_input(epoch));
        assert!(!connection.accepts_output(epoch));
    }

    #[test]
    fn security_failures_do_not_enter_automatic_backoff() {
        for failure in [
            Failure::Authentication,
            Failure::HostKey,
            Failure::MissingSession,
        ] {
            let mut connection = Connection::default();
            let epoch = connection.begin_connect();
            assert!(connection.fail(epoch, failure));
            assert_eq!(connection.state(), State::NeedsAttention(failure));
            assert!(!connection.fail(epoch, Failure::Transport));
        }
    }

    #[test]
    fn transient_failure_requires_another_restoration() {
        let mut connection = Connection::default();
        let old = connection.begin_connect();
        connection.transport_ready(old);
        connection.restored(old);
        assert!(connection.fail(old, Failure::Transport));
        assert_eq!(connection.state(), State::Backoff);
        assert!(!connection.accepts_input(old));
        let current = connection.begin_connect();
        connection.transport_ready(current);
        assert!(!connection.accepts_input(current));
    }
}
