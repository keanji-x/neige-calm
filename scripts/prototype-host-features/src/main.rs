//! Run from the repository root with a fresh output directory as the sole argument.
//! This prototype matches explicit Rust paths without resolving names or expanding macros.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned as _;
use syn::visit::{self, Visit};
use syn::{Expr, ExprCall, Lit};

fn is_path(expr: &Expr, segments: &[&str]) -> bool {
    let Expr::Path(expr) = expr else {
        return false;
    };
    expr.qself.is_none()
        && expr.path.segments.len() == segments.len()
        && expr
            .path
            .segments
            .iter()
            .zip(segments)
            .all(|(segment, name)| segment.ident == *name)
}

#[derive(Debug, PartialEq, Eq)]
enum Finding {
    Recognized(&'static str),
    NeedsReview(String),
}

fn is_zero(expr: &Expr) -> bool {
    matches!(expr, Expr::Lit(expr) if matches!(&expr.lit, Lit::Int(value) if value.base10_parse::<u32>().ok() == Some(0)))
}

fn classify(call: &ExprCall) -> Option<Finding> {
    use Finding::{NeedsReview, Recognized};

    if is_path(&call.func, &["openat2"]) || is_path(&call.func, &["nix", "fcntl", "openat2"]) {
        return Some(Recognized("openat2"));
    }
    if is_path(&call.func, &["rustix", "process", "pidfd_open"]) {
        return Some(match call.args.last() {
            Some(Expr::Call(flags))
                if call.args.len() == 2
                    && flags.args.is_empty()
                    && (is_path(&flags.func, &["PidfdFlags", "empty"])
                        || is_path(&flags.func, &["rustix", "process", "PidfdFlags", "empty"])) =>
            {
                Recognized("pidfd-open")
            }
            _ => NeedsReview("pidfd_open: flags or arguments are not recognized".into()),
        });
    }
    if is_path(&call.func, &["rustix", "process", "pidfd_send_signal"]) {
        return Some(Recognized("pidfd-send-signal"));
    }
    if !is_path(&call.func, &["libc", "syscall"]) {
        return None;
    }

    let finding = match call.args.first() {
        Some(syscall) if is_path(syscall, &["libc", "SYS_pidfd_open"]) => match call.args.get(2) {
            Some(flags) if call.args.len() == 3 && is_zero(flags) => Recognized("pidfd-open"),
            _ => NeedsReview("pidfd_open: flags or arguments are not recognized".into()),
        },
        Some(syscall) if is_path(syscall, &["libc", "SYS_pidfd_send_signal"]) => {
            match call.args.get(4) {
                Some(flags) if call.args.len() == 5 && is_zero(flags) => {
                    Recognized("pidfd-send-signal")
                }
                _ => NeedsReview("pidfd_send_signal: flags or arguments are not recognized".into()),
            }
        }
        Some(syscall) if is_path(syscall, &["libc", "SYS_close_range"]) => match call.args.get(3) {
            Some(flags)
                if call.args.len() == 4 && is_path(flags, &["libc", "CLOSE_RANGE_CLOEXEC"]) =>
            {
                Recognized("close-range-cloexec")
            }
            Some(flags) if call.args.len() == 4 && is_zero(flags) => Recognized("close-range"),
            _ => NeedsReview("close_range: flags or arguments are not recognized".into()),
        },
        Some(Expr::Path(path))
            if let Some(segment) = path.path.segments.last()
                && segment.ident.to_string().starts_with("SYS_") =>
        {
            NeedsReview(format!("unrecognized syscall constant {}", segment.ident))
        }
        Some(_) => NeedsReview("syscall number cannot be resolved statically".into()),
        None => NeedsReview("missing syscall number".into()),
    };
    Some(finding)
}

#[derive(Default)]
struct Scan(Vec<(usize, Finding)>);

impl<'ast> Visit<'ast> for Scan {
    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Some(api) = classify(call) {
            self.0.push((call.span().start().line, api));
        }
        visit::visit_expr_call(self, call);
    }
}

fn rust_sources(dir: &Path, paths: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            rust_sources(&path, paths)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            paths.push(path);
        }
    }
    Ok(())
}

fn generate(output: &Path, features: BTreeMap<&str, &str>) -> Result<(), Box<dyn Error>> {
    fs::create_dir(output)?;
    fs::create_dir(output.join("src"))?;
    let mut manifest = String::from(
        "[package]\nname = \"host-feature-demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[features]\n",
    );
    let mut code = String::from("fn main() {\n");
    for (name, version) in features {
        writeln!(manifest, "{name} = [] # Linux {version}")?;
        let condition = format!(r#"all(target_os = "linux", feature = "{name}")"#);
        writeln!(
            code,
            r#"    {{
        #[cfg({condition})]
        const ENABLED: bool = true;
        #[cfg(not({condition}))]
        const ENABLED: bool = false;
        #[cfg(target_os = "macos")]
        const _: () = assert!(!ENABLED);
        println!("{name}={{ENABLED}}");
    }}"#
        )?;
    }
    code.push_str("}\n");
    fs::write(output.join("Cargo.toml"), manifest)?;
    fs::write(output.join("src/main.rs"), code)?;
    Ok(())
}

fn linux_versions() -> Result<BTreeMap<&'static str, &'static str>, Box<dyn Error>> {
    let mut versions = BTreeMap::new();
    for line in include_str!("../linux-versions.tsv").lines() {
        if line.starts_with('#') {
            continue;
        }
        let [name, version, _source] = line.split('\t').collect::<Vec<_>>()[..] else {
            return Err(format!("invalid Linux version metadata row: {line}").into());
        };
        if versions.insert(name, version).is_some() {
            return Err(format!("duplicate Linux version metadata for {name}").into());
        }
    }
    Ok(versions)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let output = args.next().ok_or("expected a fresh output directory")?;
    if args.next().is_some() {
        return Err("expected exactly one output directory".into());
    }

    let mut paths = Vec::new();
    for entry in fs::read_dir("crates")? {
        let src = entry?.path().join("src");
        if src.is_dir() {
            rust_sources(&src, &mut paths)?;
        }
    }
    paths.sort();

    let versions = linux_versions()?;
    let mut features = BTreeMap::new();
    for path in paths {
        let source = fs::read_to_string(&path)?;
        let syntax =
            syn::parse_file(&source).map_err(|err| format!("{}: {err}", path.display()))?;
        let mut scan = Scan::default();
        scan.visit_file(&syntax);
        for (line, finding) in scan.0 {
            let path = path.display();
            match finding {
                Finding::Recognized(name) => {
                    let version = *versions
                        .get(name)
                        .ok_or_else(|| format!("missing Linux version metadata for {name}"))?;
                    println!("{path}:{line}: recognized: {name} (Linux {version})");
                    features.insert(name, version);
                }
                Finding::NeedsReview(reason) => {
                    println!("{path}:{line}: needs review: {reason}");
                }
            }
        }
    }
    generate(Path::new(&output), features)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_calls_by_syntax_and_distinguishes_flags() {
        let syntax = syn::parse_file(
            r#"
            fn example() {
                // openat2(ignored)
                let _ = "libc::syscall(libc::SYS_pidfd_open, 1, 0)";
                libc::syscall(libc::SYS_close_range, first(), last(), 0u32);
                libc::syscall(
                    libc::SYS_close_range, first(), last(), libc::CLOSE_RANGE_CLOEXEC,
                );
                libc::syscall(libc::SYS_close_range, first(), last(), flags);
                nix::fcntl::openat2(fd, path, how);
                libc::syscall(libc::SYS_pidfd_open, pid, 0);
                libc::syscall(libc::SYS_pidfd_open, pid, flags);
                libc::syscall(libc::SYS_openat, fd, path, 0);
                libc::syscall(number, 0);
                rustix::process::pidfd_open(pid, PidfdFlags::empty());
                rustix::process::pidfd_send_signal(fd, signal);
                libc::syscall(libc::SYS_pidfd_send_signal, fd, signal, std::ptr::null(), 0);
            }
            "#,
        )
        .unwrap();
        let mut scan = Scan::default();
        scan.visit_file(&syntax);
        use Finding::{NeedsReview, Recognized};
        assert_eq!(
            scan.0,
            [
                (5, Recognized("close-range")),
                (6, Recognized("close-range-cloexec")),
                (
                    9,
                    NeedsReview("close_range: flags or arguments are not recognized".into())
                ),
                (10, Recognized("openat2")),
                (11, Recognized("pidfd-open")),
                (
                    12,
                    NeedsReview("pidfd_open: flags or arguments are not recognized".into())
                ),
                (
                    13,
                    NeedsReview("unrecognized syscall constant SYS_openat".into())
                ),
                (
                    14,
                    NeedsReview("syscall number cannot be resolved statically".into())
                ),
                (15, Recognized("pidfd-open")),
                (16, Recognized("pidfd-send-signal")),
                (17, Recognized("pidfd-send-signal")),
            ]
        );
    }
}
