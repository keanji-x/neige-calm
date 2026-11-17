//! Real file reads pin text admission separately from byte-limit truncation.
use super::*;

#[tokio::test]
async fn text_readers_reject_nul_content() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("evidence.bin");
    for size in [
        21,
        MAX_READFILE_BYTES as usize,
        MAX_READFILE_BYTES as usize + 1,
    ] {
        let mut bytes = vec![b'a'; size];
        bytes[size / 2] = 0;
        std::fs::write(&path, bytes).unwrap();
        assert!(
            matches!(
                read_file_response(&path).await,
                Err(CalmError::BadRequest(_))
            ),
            "NUL in a {size}-byte file must not be returned as text"
        );
    }
}

#[tokio::test]
async fn text_readers_reject_invalid_utf8_before_truncation_boundary() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("evidence.log");
    for size in [
        123,
        MAX_READFILE_BYTES as usize,
        MAX_READFILE_BYTES as usize + 1,
    ] {
        for offset in [0, size / 2, size.min(MAX_READFILE_BYTES as usize) - 1] {
            let mut bytes = vec![b'a'; size];
            bytes[offset] = 0xff;
            std::fs::write(&path, bytes).unwrap();
            assert!(
                matches!(
                    read_file_response(&path).await,
                    Err(CalmError::BadRequest(_))
                ),
                "invalid byte at {offset} in a {size}-byte file must be rejected"
            );
        }
    }
}

#[tokio::test]
async fn text_readers_keep_complete_unicode_before_cap() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("evidence.log");
    for character in ["é", "中", "😀"] {
        for kept in 1..=character.len() {
            let prefix = MAX_READFILE_BYTES as usize - kept;
            let mut bytes = vec![b'a'; prefix];
            bytes.extend_from_slice(character.as_bytes());
            bytes.extend_from_slice(b"latest line\n");
            std::fs::write(&path, &bytes).unwrap();
            let file = read_file_response(&path).await.unwrap();
            assert!(file.truncated);
            assert_eq!(file.size, bytes.len() as u64);
            let expected = if kept == character.len() {
                format!("{}{character}", "a".repeat(prefix))
            } else {
                "a".repeat(prefix)
            };
            assert_eq!(file.text, expected);
        }
    }
}

#[tokio::test]
async fn text_readers_keep_short_exact_limit_and_truncated_logs() {
    let workspace = tempfile::tempdir().unwrap();
    for name in ["evidence", "evidence.unknown", "evidence.log"] {
        let path = workspace.path().join(name);
        for size in [
            21,
            MAX_READFILE_BYTES as usize,
            MAX_READFILE_BYTES as usize + 1,
        ] {
            let mut bytes = vec![b'a'; size];
            bytes[..6].copy_from_slice(b"FIRST\n");
            std::fs::write(&path, &bytes).unwrap();
            let file = read_file_response(&path).await.unwrap();
            assert_eq!(file.size, size as u64);
            assert_eq!(file.truncated, size as u64 > MAX_READFILE_BYTES);
            assert_eq!(
                file.text.as_bytes(),
                &bytes[..size.min(MAX_READFILE_BYTES as usize)]
            );
        }
    }
}

#[tokio::test]
async fn gitdiff_rejects_nul_in_head_and_working_text() {
    assert_diff_rejects(b"binary\0content").await;
}

#[tokio::test]
async fn gitdiff_rejects_invalid_utf8_in_oversize_head_and_working_text() {
    let mut bytes = vec![b'a'; MAX_READFILE_BYTES as usize + 1];
    bytes[123] = 0xff;
    assert_diff_rejects(&bytes).await;
}

async fn assert_diff_rejects(binary: &[u8]) {
    for binary_in_head in [true, false] {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("evidence.log");
        let plain = b"readable text\n".as_slice();
        tests::git(workspace.path(), &["init"]);
        std::fs::write(&path, if binary_in_head { binary } else { plain }).unwrap();
        tests::git(workspace.path(), &["add", "evidence.log"]);
        tests::git(workspace.path(), &["commit", "-m", "fixture"]);
        std::fs::write(&path, if binary_in_head { plain } else { binary }).unwrap();
        assert!(
            matches!(
                git_diff_response(&path, None).await,
                Err(CalmError::BadRequest(_))
            ),
            "binary content must be rejected with binary_in_head={binary_in_head}"
        );
    }
}
