use std::ffi::OsString;
use std::process::Stdio;

use calm_server::operation::forge_action_adapter::{
    FORGE_CREDENTIAL_ENV_KEYS, credentials::write_credential_payload, render_forge_wrapper,
};
use tokio::io::AsyncWriteExt;

async fn wrapper(script: &str) -> (tempfile::TempDir, tokio::process::Child) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.args([
        "-c",
        &render_forge_wrapper(&["/bin/sh".into(), "-c".into(), script.into()]).unwrap(),
    ])
    .env_clear()
    .env("PATH", "/usr/bin:/bin")
    .env("NEIGE_FORGE_RESULT_PATH", tmp.path().join("result"))
    .current_dir(tmp.path())
    .stdin(Stdio::piped());
    (tmp, cmd.spawn().unwrap())
}

#[tokio::test]
async fn c3_credential_payload_round_trips_shell_values() {
    for credential in FORGE_CREDENTIAL_ENV_KEYS {
        for value in [
            None,
            Some(""),
            Some(" \t  "),
            Some(r"a\b\\c"),
            Some("'\";$() `touch injected` * ? ="),
        ] {
            let script = format!(
                "[ \"${{{credential}+x}}\" = x ] && printf 'set:%s' \"${credential}\" || printf unset"
            );
            let (tmp, mut child) = wrapper(&script).await;
            write_credential_payload(child.stdin.as_mut().unwrap(), |key| {
                (key == *credential)
                    .then(|| value.map(OsString::from))
                    .flatten()
            })
            .await
            .unwrap();
            drop(child.stdin.take());
            assert!(child.wait().await.unwrap().success());
            let actual = std::fs::read_to_string(tmp.path().join("result.stdout")).unwrap();
            assert_eq!(actual, value.map_or("unset".into(), |v| format!("set:{v}")));
            assert!(!tmp.path().join("injected").exists());
        }
    }
}

#[tokio::test]
async fn c3_partial_credential_payload_never_runs_action() {
    let mut full = Vec::new();
    write_credential_payload(&mut full, |_| None).await.unwrap();
    let mut cases: Vec<Vec<u8>> = (0..full.len()).map(|n| full[..n].to_vec()).collect();
    cases.push([&full[..full.len() - 3], b"no\n"].concat());
    for index in 0..FORGE_CREDENTIAL_ENV_KEYS.len() {
        let mut invalid = full.clone();
        invalid[index * 2] = b'2';
        cases.push(invalid);
    }
    for bytes in cases {
        let (tmp, mut child) = wrapper("touch ran").await;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(&bytes)
            .await
            .unwrap();
        drop(child.stdin.take());
        assert_eq!(child.wait().await.unwrap().code(), Some(75));
        assert!(!tmp.path().join("ran").exists());
        assert!(!tmp.path().join("result.stdout").exists());
    }
}

#[tokio::test]
async fn c3_refused_release_writes_no_byte() {
    // Put invalid input last: even valid preceding credentials must not be released.
    for (value, all_keys) in [
        ("bad\nvalue".to_string(), false),
        ("bad\0value".to_string(), false),
        ("x".repeat(16 * 1024), false),
        (
            "x".repeat(16 * 1024 / FORGE_CREDENTIAL_ENV_KEYS.len() + 1),
            true,
        ),
    ] {
        let mut written = Vec::new();
        let result = write_credential_payload(&mut written, |key| {
            Some(
                if all_keys || key == *FORGE_CREDENTIAL_ENV_KEYS.last().unwrap() {
                    value.clone().into()
                } else {
                    "valid".into()
                },
            )
        })
        .await;
        assert!(result.is_err());
        assert!(written.is_empty(), "refused payload wrote bytes");
        assert!(!result.unwrap_err().to_string().contains(&value));
    }
}
