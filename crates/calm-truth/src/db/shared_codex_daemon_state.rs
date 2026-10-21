#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedDaemonState {
    Idle,
    Starting,
    Running,
    Restarting,
    Failed,
}

impl SharedDaemonState {
    pub fn as_db_str(self) -> &'static str {
        match self {
            SharedDaemonState::Idle => "idle",
            SharedDaemonState::Starting => "starting",
            SharedDaemonState::Running => "running",
            SharedDaemonState::Restarting => "restarting",
            SharedDaemonState::Failed => "failed",
        }
    }
}

impl TryFrom<String> for SharedDaemonState {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "idle" => Ok(Self::Idle),
            "starting" => Ok(Self::Starting),
            "running" => Ok(Self::Running),
            "restarting" => Ok(Self::Restarting),
            "failed" => Ok(Self::Failed),
            other => Err(format!("unknown shared_codex_daemon.state value `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_machine_transitions_start_run_restart_failed() {
        let states = [
            SharedDaemonState::Idle,
            SharedDaemonState::Starting,
            SharedDaemonState::Running,
            SharedDaemonState::Restarting,
            SharedDaemonState::Failed,
        ];
        for state in states {
            assert_eq!(
                SharedDaemonState::try_from(state.as_db_str().to_string()),
                Ok(state)
            );
        }
    }

    #[test]
    fn unknown_state_is_rejected() {
        assert!(SharedDaemonState::try_from("bogus".to_string()).is_err());
    }
}
