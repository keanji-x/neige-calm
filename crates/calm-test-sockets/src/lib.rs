//! 测试专用：给 unix domain socket 发放「短路径」。
//!
//! `sun_path` 只有 108 字节（含结尾的 NUL，实际可用 107）。测试里习惯把
//! socket 建在 `TempDir` 下，而 `TempDir` 默认落在 `$TMPDIR` —— 自托管
//! runner 上 `TMPDIR=/home/runner/actions-runner-neige-calm/_work/_temp`
//! 就已经 49 字节，再加 `TempDir` 自己的 `/.tmpXXXXXX`（11 字节）只剩
//! 47 字节给socket 名字。短路径下全绿、长 `TMPDIR` 下 `InvalidInput:
//! "path must be shorter than SUN_LEN"` —— #1439 就是这么红的。
//!
//! 所以 socket 目录不能问 `$TMPDIR` 要，必须自己钉在一个短基址上。
//! `socket_dir()` 把目录钉在 `/tmp/nsk-<uid>` 下（同 uid 私有，
//! `tempfile` 的随机后缀保证并发测试进程之间不撞名），
//! `socket_path()` 在 bind 之前就把超限的路径连同它的字节数打出来。

use std::io;
use std::path::{Path, PathBuf};

pub use tempfile::TempDir;

/// `sun_path` 是 108 字节且必须以 NUL 结尾，故路径本身最多 107 字节。
pub const MAX_SOCKET_PATH_BYTES: usize = 107;

/// 短基址：`/tmp/nsk-<uid>`（13 字节左右），刻意不读 `$TMPDIR`。
/// `/tmp` 不可写时退回 `std::env::temp_dir()` —— 那时长度不再有保证，
/// 但 [`socket_path`] 的断言仍会把问题说清楚。
fn base_dir() -> PathBuf {
    // SAFETY: getuid() 无参数、不会失败、无内存副作用。
    let uid = unsafe { libc::getuid() };
    let base = PathBuf::from(format!("/tmp/nsk-{uid}"));
    match std::fs::create_dir_all(&base) {
        Ok(()) => base,
        Err(_) => std::env::temp_dir(),
    }
}

/// 一个只存放 socket 的临时目录，路径长度与 `$TMPDIR` 无关。
///
/// `prefix` 请保持很短（几个字符），它直接计入 `sun_path` 预算。
#[track_caller]
pub fn socket_dir(prefix: &str) -> TempDir {
    try_socket_dir(prefix)
        .unwrap_or_else(|e| panic!("create short-path socket dir (prefix {prefix:?}): {e}"))
}

/// [`socket_dir`] 的可失败版本，给不想 panic 的调用方。
pub fn try_socket_dir(prefix: &str) -> io::Result<TempDir> {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(base_dir())
}

/// 在 `dir` 下拼出 socket 路径，并在 bind 之前校验它塞得进 `sun_path`。
///
/// 失败信息带上算出来的路径和它的字节数 —— 裸 `unwrap()` 只会说
/// "path must be shorter than SUN_LEN"，不说是哪条路径、多长。
#[track_caller]
pub fn socket_path(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    assert_fits(&path);
    path
}

/// 校验 `path` 塞得进 `sun_path`；超限时连同字节数一起 panic。
#[track_caller]
pub fn assert_fits(path: &Path) {
    let len = path.as_os_str().as_encoded_bytes().len();
    assert!(
        len <= MAX_SOCKET_PATH_BYTES,
        "unix socket path is {len} bytes, over the {MAX_SOCKET_PATH_BYTES}-byte \
         sun_path limit: {}",
        path.display()
    );
}

/// bind 一个 `std` 的 `UnixListener`，失败信息自解释（路径 + 字节数 + errno）。
#[track_caller]
pub fn bind(path: &Path) -> std::os::unix::net::UnixListener {
    assert_fits(path);
    std::os::unix::net::UnixListener::bind(path).unwrap_or_else(|e| {
        panic!(
            "bind unix socket at {} ({} bytes): {e}",
            path.display(),
            path.as_os_str().as_encoded_bytes().len()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_dir_is_short_regardless_of_tmpdir() {
        let dir = socket_dir("t");
        // 目录本身要短到还能容下一个像样的 socket 名字。
        let len = dir.path().as_os_str().as_encoded_bytes().len();
        assert!(
            len < 40,
            "socket dir too long ({len}): {}",
            dir.path().display()
        );
    }

    #[test]
    fn a_bound_socket_round_trips() {
        let dir = socket_dir("t");
        let path = socket_path(dir.path(), "probe.sock");
        let listener = bind(&path);
        drop(listener);
        assert!(path.exists());
    }

    #[test]
    #[should_panic(expected = "sun_path limit")]
    fn an_over_long_path_names_itself() {
        assert_fits(Path::new(&format!("/tmp/{}", "a".repeat(200))));
    }
}
