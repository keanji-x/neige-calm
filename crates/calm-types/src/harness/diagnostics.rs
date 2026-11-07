//! Closed ACP turn diagnostics safe to retain across projection and recovery.
/// Kernel-owned display diagnostics. No native message, tool title or input is retained here.
#[derive(Clone, Copy, Debug)]
pub enum AcpDiagnostic {
    Never,
    Rejected,
    NativeFull,
    SilentToolFailure,
}
impl AcpDiagnostic {
    pub fn diagnostic(self) -> (&'static str, &'static str) {
        match self {
            Self::Never => (
                "acp_permission_denied",
                "Neige declined a tool request under `never`. The requested tool needs permission this conversation does not grant.",
            ),
            Self::SilentToolFailure => (
                "acp_tool_failure",
                "The assistant stopped after a tool failed and did not reply.",
            ),
            Self::NativeFull => (
                "acp_permission_unexpected",
                "The agent requested permission despite its native full configuration. Neige declined the unexpected request.",
            ),
            Self::Rejected => (
                "acp_permission_rejected",
                "The tool permission request was declined. The assistant stopped without replying.",
            ),
        }
    }
}

/// Only these closed codes may survive canonical recovery; always replace the native text.
pub fn terminal_diagnostic(code: &str) -> Option<(&'static str, &'static str)> {
    [
        AcpDiagnostic::Never,
        AcpDiagnostic::Rejected,
        AcpDiagnostic::NativeFull,
        AcpDiagnostic::SilentToolFailure,
    ]
    .into_iter()
    .map(AcpDiagnostic::diagnostic)
    .find(|(known, _)| *known == code)
}
