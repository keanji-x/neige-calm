use super::*;

/// Session-scoped declarations observed from setup, idle traffic and the routed driver.
/// Replacing a menu replaces that session's catalog, including an empty menu.
#[derive(Clone, Default)]
pub(in crate::acp_planner) struct Commands(
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>>,
);
impl Commands {
    pub(in crate::acp_planner) fn contains(&self, session: &str, command: &str) -> bool {
        self.0
            .lock()
            .expect("ACP commands")
            .get(session)
            .is_some_and(|names| names.iter().any(|name| name == command))
    }
    pub(in crate::acp_planner) fn declared(&self, session: &str) -> bool {
        self.0.lock().expect("ACP commands").contains_key(session)
    }
    /// An attach response can precede its menu. Only bound sessions may populate
    /// the catalog while waiting; a sibling's declaration cannot release this wait.
    pub(in crate::acp_planner) async fn wait_for_declaration(
        &self,
        connection: &mut Connection,
        session: &str,
        bound_sessions: &[&str],
    ) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !self.declared(session) {
                let frame = connection.incoming.recv().await.ok_or_else(|| {
                    CalmError::Conflict("ACP command catalog was not declared".into())
                })?;
                if let Incoming::Notification { params, .. } = &frame
                    && params["sessionId"]
                        .as_str()
                        .is_some_and(|native| bound_sessions.contains(&native))
                {
                    self.observe(&frame);
                }
                if let Incoming::Request { id, method, .. } = frame {
                    refuse(&connection.client, id, &method)
                        .await
                        .map_err(|error| CalmError::Conflict(error.to_string()))?;
                }
            }
            Ok(())
        })
        .await
        .map_err(|_| CalmError::Conflict("ACP command catalog timed out".into()))?
    }
    pub(in crate::acp_planner) fn retain_sessions(
        &self,
        sessions: &std::collections::HashSet<&str>,
    ) {
        self.0
            .lock()
            .expect("ACP commands")
            .retain(|id, _| sessions.contains(id.as_str()));
    }
    pub(in crate::acp_planner) fn observe_for(&self, session: &str, frame: &Incoming) {
        if let Incoming::Notification { params, .. } = frame
            && params["sessionId"].as_str() == Some(session)
        {
            self.observe(frame);
        }
    }
    pub(in crate::acp_planner) fn forget(&self, session: &str) {
        self.0.lock().expect("ACP commands").remove(session);
    }
    pub(in crate::acp_planner) fn observe(&self, frame: &Incoming) {
        let Incoming::Notification { method, params } = frame else {
            return;
        };
        if method != "session/update"
            || params["update"]["sessionUpdate"] != "available_commands_update"
        {
            return;
        }
        let Some(session) = params["sessionId"].as_str() else {
            return;
        };
        let names = params["update"]["availableCommands"]
            .as_array()
            .map(|available| {
                available
                    .iter()
                    .filter_map(|command| command["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        self.0
            .lock()
            .expect("ACP commands")
            .insert(session.to_owned(), names);
    }
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
