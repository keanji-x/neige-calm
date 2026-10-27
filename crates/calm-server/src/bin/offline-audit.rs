//! Explicit stopped-backup audit; does not enter server startup.
#[cfg(target_os = "linux")]
use calm_server::offline_audit::export::{AuditInput, export, scan};
#[cfg(target_os = "linux")]
use calm_truth::conversation_upgrade::{FrozenBackup, SourceDeclaration};
#[cfg(target_os = "linux")]
use std::{ffi::OsString, os::unix::ffi::OsStrExt, path::PathBuf};

#[cfg(target_os = "linux")]
fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let command = args.next().ok_or("expected scan or export")?;
    let mut value = |flag: &str| -> Result<OsString, String> {
        if args.next().as_deref() != Some(std::ffi::OsStr::new(flag)) {
            return Err(format!("expected {flag}"));
        }
        args.next().ok_or(format!("missing {flag} value"))
    };
    let backup = PathBuf::from(value("--backup")?);
    let operator = value("--operator")?
        .into_string()
        .map_err(|_| "operator must be UTF-8")?;
    let freeze_evidence = value("--freeze-evidence")?
        .into_string()
        .map_err(|_| "evidence must be UTF-8")?;
    let complete_set_evidence = value("--complete-set-evidence")?
        .into_string()
        .map_err(|_| "evidence must be UTF-8")?;
    let output = if command == "export" {
        Some(PathBuf::from(value("--output")?))
    } else if command == "scan" {
        None
    } else {
        return Err("expected scan or export".into());
    };
    let mut sources = Vec::new();
    let mut empty_set = false;
    while let Some(flag) = args.next() {
        if flag == "--empty-owned-set" {
            empty_set = true;
            continue;
        }
        if flag != "--owned-root" {
            return Err("expected --owned-root PATH OWNERSHIP FREEZE_EVIDENCE".into());
        }
        let path = args.next().ok_or("missing root path")?.as_bytes().to_vec();
        let ownership = args
            .next()
            .ok_or("missing ownership")?
            .into_string()
            .map_err(|_| "ownership must be UTF-8")?;
        let freeze_evidence = args
            .next()
            .ok_or("missing root freeze evidence")?
            .into_string()
            .map_err(|_| "evidence must be UTF-8")?;
        sources.push(SourceDeclaration {
            path,
            ownership,
            freeze_evidence,
        });
    }
    if sources.is_empty() != empty_set {
        return Err("UnsupportedInput: declare --owned-root or --empty-owned-set".into());
    }
    let input = AuditInput {
        backup: FrozenBackup {
            path: backup,
            operator,
            freeze_evidence,
        },
        complete_set_evidence,
        sources,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    if let Some(output) = output {
        let root = export(&runtime, input, output).map_err(|e| format!("{e:?}"))?;
        println!("BlockedAudit PublishedDurable {root:02x?}");
    } else {
        use std::io::Write;
        std::io::stdout()
            .write_all(&scan(&runtime, input).map_err(|e| format!("{e:?}"))?)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn run() -> Result<(), String> {
    Err("UnsupportedPlatform: offline-audit requires Linux".into())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
