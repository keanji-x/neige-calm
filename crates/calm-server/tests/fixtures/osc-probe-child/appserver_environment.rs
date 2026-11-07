//! Report the real fixture process environment without relying on Linux procfs.
use std::os::unix::ffi::OsStringExt;

pub(super) fn write_if_requested() {
    let Some(path) = std::env::var_os("FAKE_CODEX_ENV_DUMP") else {
        return;
    };
    let bytes: Vec<u8> = std::env::vars_os()
        .flat_map(|(key, value)| {
            let mut bytes = key.into_vec();
            bytes.push(b'=');
            bytes.extend(value.into_vec());
            bytes.push(0);
            bytes
        })
        .collect();
    std::fs::write(path, bytes).expect("report actual fake app-server environment");
}
