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

type Capability = (&'static str, &'static str);

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

fn capability(call: &ExprCall) -> Option<Capability> {
    if is_path(&call.func, &["openat2"]) || is_path(&call.func, &["nix", "fcntl", "openat2"]) {
        return Some(("openat2", "5.6"));
    }
    if !is_path(&call.func, &["libc", "syscall"]) {
        return None;
    }

    let syscall = call.args.first()?;
    if is_path(syscall, &["libc", "SYS_pidfd_open"]) {
        return Some(("pidfd-open", "5.3"));
    }
    if !is_path(syscall, &["libc", "SYS_close_range"]) || call.args.len() != 4 {
        return None;
    }

    let flags = &call.args[3];
    if is_path(flags, &["libc", "CLOSE_RANGE_CLOEXEC"]) {
        Some(("close-range-cloexec", "5.11"))
    } else if matches!(flags, Expr::Lit(expr) if matches!(&expr.lit, Lit::Int(value) if value.base10_parse::<u32>().ok() == Some(0)))
    {
        Some(("close-range", "5.9"))
    } else {
        None
    }
}

#[derive(Default)]
struct Scan(Vec<(usize, Capability)>);

impl<'ast> Visit<'ast> for Scan {
    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Some(api) = capability(call) {
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

    let mut features = BTreeMap::new();
    for path in paths {
        let source = fs::read_to_string(&path)?;
        let syntax =
            syn::parse_file(&source).map_err(|err| format!("{}: {err}", path.display()))?;
        let mut scan = Scan::default();
        scan.visit_file(&syntax);
        for (line, (name, version)) in scan.0 {
            println!("{}:{line}: {name} (Linux {version})", path.display());
            features.insert(name, version);
        }
    }
    if features.is_empty() {
        return Err("no known calls found in crates/*/src; run from the repository root".into());
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
            }
            "#,
        )
        .unwrap();
        let mut scan = Scan::default();
        scan.visit_file(&syntax);
        assert_eq!(
            scan.0,
            [
                (5, ("close-range", "5.9")),
                (6, ("close-range-cloexec", "5.11")),
                (10, ("openat2", "5.6")),
                (11, ("pidfd-open", "5.3")),
            ]
        );
    }
}
