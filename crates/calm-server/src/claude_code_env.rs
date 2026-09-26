//! Environment every Claude Code process the kernel spawns carries, whatever spawns it.

/// Auto-memory off (#1814). A kernel-spawned Claude shares the owner's config dir, so its
/// auto-memory is the owner's: it would load the owner's notes and could write persistent
/// instructions every owner session loads.
pub(crate) const DISABLE_AUTO_MEMORY: (&str, &str) = ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1");
