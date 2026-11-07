//! Documented AGY CLI settings profiles share account authentication and native history.
//! Never rewrite the operator's settings or infer the native conversation storage layout.
use super::ModeLaunch;
use crate::acp_planner::config::AcpAgentConfig;
use crate::error::{CalmError, Result};
use crate::planner_permission_mode::PlannerPermissionMode;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;

pub(super) const SANDBOXED: ModeLaunch = ModeLaunch {
    env: &[],
    agent_options: &["--cli_dir_name", "neige-sandboxed-v1", "--sandbox=true"],
};
pub(super) const UNRESTRICTED: ModeLaunch = ModeLaunch {
    env: &[],
    agent_options: &["--cli_dir_name", "neige-unrestricted-v1", "--sandbox=false"],
};

pub(super) fn prepare(config: &AcpAgentConfig, mode: PlannerPermissionMode) -> Result<()> {
    let home = config
        .env
        .get("HOME")
        .map(Path::new)
        .filter(|home| home.is_absolute())
        .ok_or_else(|| {
            CalmError::BadRequest("Antigravity requires an absolute registered HOME".into())
        })?;
    let root = home.join(".gemini/antigravity-cli");
    let full = mode == PlannerPermissionMode::Full;
    let settings = policy(read_settings(&root.join("settings.json"))?, full)?;
    let name = if full {
        "neige-unrestricted-v1"
    } else {
        "neige-sandboxed-v1"
    };
    let directory = root.join(name);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    if !std::fs::symlink_metadata(&directory)?.file_type().is_dir() {
        return Err(CalmError::BadRequest(
            "Antigravity managed settings directory is not a directory".into(),
        ));
    }
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    let mut file = tempfile::NamedTempFile::new_in(&directory)?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(&mut file, &settings)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(directory.join("settings.json"))
        .map_err(|error| error.error)?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

fn read_settings(path: &Path) -> Result<Value> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(error) => return Err(error.into()),
    };
    const LIMIT: u64 = 1024 * 1024;
    if !file.metadata()?.is_file() || file.metadata()?.len() > LIMIT {
        return Err(CalmError::BadRequest(
            "Antigravity settings exceed the supported size".into(),
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(CalmError::BadRequest(
            "Antigravity settings exceed the supported size".into(),
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn policy(mut settings: Value, full: bool) -> Result<Value> {
    let object = settings
        .as_object_mut()
        .ok_or_else(|| CalmError::BadRequest("Antigravity settings must be an object".into()))?;
    if let Some(permissions) = object.get("permissions") {
        let rules = permissions.as_object().ok_or_else(|| {
            CalmError::BadRequest("Antigravity permissions must be an object".into())
        })?;
        if rules
            .values()
            .any(|value| value.as_array().is_none_or(|rules| !rules.is_empty()))
        {
            // Real ACP probes execute a sandbox command despite deny command(*) in
            // an isolated CLI profile. Do not pretend copying rules enforces them.
            return Err(CalmError::BadRequest(
                "Antigravity custom native permission rules cannot be enforced in this managed CLI profile".into()));
        }
    }
    object.insert("agentMode".into(), json!(""));
    object.insert("enableTerminalSandbox".into(), json!(!full));
    object.insert(
        "toolPermission".into(),
        json!(if full {
            "request-review"
        } else {
            "proceed-in-sandbox"
        }),
    );
    object.insert("allowNonWorkspaceAccess".into(), json!(full));
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn antigravity_native_policy_rejects_unenforced_rules_and_pins_sandbox_policy() {
        let original = json!({"agentMode":"auto", "enableTerminalSandbox":false,
            "toolPermission":"always-proceed", "allowNonWorkspaceAccess":true,
            "colorScheme":"dark"});
        let sandbox = policy(original.clone(), false).unwrap();
        assert_eq!(sandbox["toolPermission"], "proceed-in-sandbox");
        assert_eq!(sandbox["agentMode"], "");
        assert_eq!(sandbox["enableTerminalSandbox"], true);
        assert_eq!(sandbox["allowNonWorkspaceAccess"], false);
        let full = policy(original.clone(), true).unwrap();
        assert_eq!(full["enableTerminalSandbox"], false);
        assert_eq!(full["toolPermission"], "request-review");
        assert_eq!(sandbox["colorScheme"], original["colorScheme"]);
        for rule in ["allow", "ask", "deny"] {
            for full in [false, true] {
                assert!(matches!(
                    policy(json!({"permissions":{rule:["command(*)"]}}), full),
                    Err(CalmError::BadRequest(_))
                ));
            }
        }
        assert!(policy(json!({"permissions":{"deny":"invalid"}}), false).is_err());
        assert!(policy(json!([]), false).is_err());
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use calm_types::runtime::AgentProvider;
    use std::collections::BTreeMap;
    #[test]
    fn antigravity_native_settings_are_private_and_leave_operator_file_unchanged() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".gemini/antigravity-cli");
        std::fs::create_dir_all(&root).unwrap();
        let original = br#"{"agentMode":"auto","colorScheme":"dark"}"#;
        std::fs::write(root.join("settings.json"), original).unwrap();
        let mut config = AcpAgentConfig {
            provider: AgentProvider::Antigravity,
            command: "/unused/agy".into(),
            args: vec![],
            env: BTreeMap::new(),
            min_agent_version: semver::Version::new(1, 5, 3),
            default_permission_mode: None,
        };
        assert!(matches!(
            prepare(&config, PlannerPermissionMode::Never),
            Err(CalmError::BadRequest(_))
        ));
        config
            .env
            .insert("HOME".into(), home.path().to_string_lossy().into_owned());
        for (mode, name, sandbox) in [
            (PlannerPermissionMode::Never, "neige-sandboxed-v1", true),
            (PlannerPermissionMode::Full, "neige-unrestricted-v1", false),
            (PlannerPermissionMode::Ask, "neige-sandboxed-v1", true),
        ] {
            prepare(&config, mode).unwrap();
            let directory = root.join(name);
            let output = directory.join("settings.json");
            let settings: Value = serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
            assert_eq!(settings["enableTerminalSandbox"], sandbox);
            assert_eq!(settings["colorScheme"], "dark");
            assert_eq!(
                std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(output).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(std::fs::read(root.join("settings.json")).unwrap(), original);
        }
        let managed = root.join("neige-sandboxed-v1");
        std::fs::remove_file(managed.join("settings.json")).unwrap();
        std::fs::remove_dir(&managed).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), &managed).unwrap();
        assert!(matches!(
            prepare(&config, PlannerPermissionMode::Never),
            Err(CalmError::BadRequest(_))
        ));
        assert!(!outside.path().join("settings.json").exists());
    }
}
