use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use anyhow::Context;

pub(super) fn password(path: Option<&Path>) -> anyhow::Result<String> {
    let password = match path {
        Some(path) => {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
                .context("open password file (must be a private regular file, not a symlink)")?;
            let metadata = file.metadata()?;
            if !metadata.is_file()
                || metadata.permissions().mode() & 0o077 != 0
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                anyhow::bail!(
                    "password file must be a private regular file owned by the current user (chmod 600)"
                );
            }
            let mut text = String::new();
            file.take(1027)
                .read_to_string(&mut text)
                .context("read password file")?;
            if text.len() > 1026 {
                anyhow::bail!("password file is too large");
            }
            text.trim_end_matches(['\r', '\n']).to_owned()
        }
        None => {
            let mut terminal = OpenOptions::new().read(true).write(true).open("/dev/tty")
                .context("interactive terminal required; use --password-file with a private file for unattended setup")?;
            let first = prompt(
                &mut terminal,
                "Choose an owner password (at least 12 characters): ",
            )?;
            let repeated = prompt(&mut terminal, "Repeat the password: ")?;
            if first != repeated {
                anyhow::bail!("passwords do not match; nothing was installed");
            }
            first
        }
    };
    if !(12..=256).contains(&password.chars().count()) || password.chars().any(char::is_control) {
        anyhow::bail!("password must contain 12–256 characters and no control characters");
    }
    Ok(password)
}

fn prompt(terminal: &mut File, message: &str) -> anyhow::Result<String> {
    let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(terminal.as_raw_fd(), original.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("read terminal settings");
    }
    let original = unsafe { original.assume_init() };
    let mut hidden = original;
    // Consume Ctrl-C ourselves so cancellation always restores echo before
    // returning. Canonical signal handling would terminate without running Drop.
    hidden.c_lflag &= !(libc::ECHO | libc::ICANON | libc::ISIG);
    hidden.c_cc[libc::VMIN] = 1;
    hidden.c_cc[libc::VTIME] = 0;
    if unsafe { libc::tcsetattr(terminal.as_raw_fd(), libc::TCSAFLUSH, &hidden) } != 0 {
        return Err(std::io::Error::last_os_error()).context("hide password input");
    }
    struct Restore(i32, libc::termios);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                libc::tcsetattr(self.0, libc::TCSAFLUSH, &self.1);
            }
        }
    }
    let restore = Restore(terminal.as_raw_fd(), original);
    terminal.write_all(message.as_bytes())?;
    terminal.flush()?;
    let mut password = Vec::new();
    loop {
        let mut byte = [0];
        terminal.read_exact(&mut byte)?;
        match byte[0] {
            b'\n' | b'\r' => break,
            3 | 4 => anyhow::bail!("password entry canceled; nothing was installed"),
            8 | 127 => while password.pop().is_some_and(|byte| byte & 0xc0 == 0x80) {},
            byte if byte.is_ascii_control() => {
                anyhow::bail!("password must not contain control characters")
            }
            byte => {
                password.push(byte);
                if password.len() > 1024 {
                    anyhow::bail!("password is too long");
                }
            }
        }
    }
    drop(restore);
    terminal.write_all(b"\n")?;
    String::from_utf8(password).context("password must be UTF-8")
}
