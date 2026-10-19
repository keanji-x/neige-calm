//! Concrete child ownership shared by execution setup/monitor and read-only observers.
use std::process::{Child, ExitStatus};

pub(super) struct OwnedChild(Option<Child>);
impl OwnedChild {
    pub(super) fn new(child: Child) -> Self {
        Self(Some(child))
    }
    pub(super) fn id(&self) -> u32 {
        self.0.as_ref().expect("owned child not yet reaped").id()
    }
    pub(super) fn wait(&mut self) -> std::io::Result<ExitStatus> {
        let status = self
            .0
            .as_mut()
            .expect("owned child not yet reaped")
            .wait()?;
        self.0.take();
        Ok(status)
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            // Unreaped Child pins this exact PID. No alias lookup or wildcard adoption occurs.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
