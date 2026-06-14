pub trait CodexDaemonProbe: Send + Sync {
    fn is_running(&self) -> bool;
    fn active_turn_id_for_thread(&self, thread_id: &str) -> Option<String>;
    fn remote_uri(&self) -> String;
}
