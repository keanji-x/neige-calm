use super::*;
use crate::offline_audit::BuildError;
use crate::planner_attachments::dir::read_fault;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn conversation_upgrade_fs_build_consumes_all_inventory_failures_even_if_ignored() {
    // SAFETY: geteuid has no arguments and does not access caller memory.
    assert_ne!(unsafe { nix::libc::geteuid() }, 0);
    for kind in [
        "directory",
        "listing",
        "changed",
        "callback",
        "failed_callback",
        "read",
    ] {
        for ignore in [false, true] {
            let source = tempfile::tempdir().unwrap();
            let destination = tempfile::tempdir().unwrap();
            fs::write(source.path().join("a"), b"copied first").unwrap();
            if kind == "directory" {
                fs::create_dir(source.path().join("z")).unwrap();
                fs::set_permissions(source.path().join("z"), fs::Permissions::from_mode(0o0))
                    .unwrap();
            }
            let root = TrustedReadRoot::open_absolute(source.path()).unwrap();
            let output = super::tests::publisher(destination.path(), "final");
            let mut fault = None;
            let result = output.build(|writer| {
                let inventory = writer.inventory_owned_tree(&root, |writer, path, input| {
                    writer
                        .write_file(path, input)
                        .map_err(|e| io::Error::other(format!("{e:?}")))?;
                    match kind {
                        "listing" => {
                            fs::set_permissions(source.path(), fs::Permissions::from_mode(0o0))?
                        }
                        "changed" => fs::write(source.path().join("a"), b"changed")?,
                        "failed_callback" => {
                            assert!(matches!(
                                writer.write_file(path, &mut &b"x"[..]),
                                Err(PublicationError::Partial { .. })
                            ));
                            return Err(io::ErrorKind::BrokenPipe.into());
                        }
                        "callback" => return Err(io::ErrorKind::BrokenPipe.into()),
                        "read" => fault = Some(read_fault::install([Some(nix::libc::EIO)])),
                        _ => (),
                    }
                    Ok(())
                });
                if ignore {
                    assert!(inventory.is_err(), "{kind}");
                    Ok(())
                } else {
                    inventory.map(|_| ())
                }
            });
            drop(fault);
            fs::set_permissions(source.path(), fs::Permissions::from_mode(0o700)).unwrap();
            if kind == "directory" {
                fs::set_permissions(source.path().join("z"), fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            if ignore {
                assert!(
                    matches!(
                        result,
                        Err(BuildError::Publication(PublicationError::AlreadyFailed))
                    ),
                    "{kind}: {result:?}"
                );
            } else {
                let Err(BuildError::Build(partial)) = result else {
                    panic!("{kind}: {result:?}")
                };
                match kind {
                    "directory" | "listing" => {
                        assert_eq!(partial.error.kind(), io::ErrorKind::PermissionDenied)
                    }
                    "callback" | "failed_callback" => {
                        assert_eq!(partial.relative, Path::new("a"));
                        assert_eq!(partial.error.kind(), io::ErrorKind::BrokenPipe)
                    }
                    "read" => assert_eq!(partial.error.raw_os_error(), Some(nix::libc::EIO)),
                    _ => assert_eq!(partial.relative, Path::new("a")),
                }
            }
            assert!(
                !destination.path().join("final").exists(),
                "{kind}/{ignore}"
            );
            let temporary = fs::read_dir(destination.path())
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            assert_eq!(fs::read(temporary.join("a")).unwrap(), b"copied first");
        }
    }
}

#[test]
fn conversation_upgrade_fs_fallible_build_keeps_all_files_provisional() {
    let destination = tempfile::tempdir().unwrap();
    let output = super::tests::publisher(destination.path(), "final");
    let result = output.build(|writer| {
        writer
            .write_file(Path::new("owned"), &mut &b"provisional"[..])
            .unwrap();
        Err::<(), _>(io::ErrorKind::UnexpectedEof)
    });
    assert!(matches!(
        result,
        Err(BuildError::Build(io::ErrorKind::UnexpectedEof))
    ));
    assert!(!destination.path().join("final").exists());
}

#[test]
fn conversation_upgrade_fs_failed_writer_rejects_later_inventory() {
    let mut observed = Vec::new();
    for (during, prior) in [
        (false, "write"),
        (false, "inventory"),
        (true, "write"),
        (true, "inventory"),
    ] {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        fs::write(source.path().join("raw"), b"source").unwrap();
        fs::write(source.path().join("z"), b"second source").unwrap();
        let root = TrustedReadRoot::open_absolute(source.path()).unwrap();
        let output = Publisher::new(
            TrustedReadRoot::open_absolute(destination.path()).unwrap(),
            OsStr::new("final"),
            &[],
        )
        .unwrap();
        let result = output.build(|writer| {
            let fail = |writer: &mut BuildWriter<'_>| {
                if prior == "write" {
                    assert!(matches!(
                        writer.write_file(Path::new("missing/file"), &mut &b"x"[..]),
                        Err(PublicationError::Partial { .. })
                    ));
                } else {
                    let partial = writer
                        .inventory_owned_tree(
                            &root,
                            |_, _, _| Err(io::ErrorKind::BrokenPipe.into()),
                        )
                        .unwrap_err();
                    assert_eq!(partial.relative, Path::new("raw"));
                    assert_eq!(partial.error.kind(), io::ErrorKind::BrokenPipe);
                }
            };
            if !during {
                fail(writer);
            }
            let mut callbacks = 0;
            let later = writer.inventory_owned_tree(&root, |writer, _, input| {
                callbacks += 1;
                io::copy(input, &mut io::sink())?;
                if during && callbacks == 1 {
                    fail(writer);
                }
                Ok(())
            });
            observed.push((during, prior, later, callbacks));
            assert!(matches!(
                writer.add_directory(Path::new("later")),
                Err(PublicationError::AlreadyFailed)
            ));
            assert!(matches!(
                writer.write_file(Path::new("later"), &mut &b"x"[..]),
                Err(PublicationError::AlreadyFailed)
            ));
            Ok::<_, ()>(())
        });
        assert!(matches!(
            result,
            Err(BuildError::Publication(PublicationError::AlreadyFailed))
        ));
        assert!(!destination.path().join("final").exists());
    }
    assert!(
        observed.iter().all(|(during, _, result, callbacks)| {
            matches!(result, Err(partial)
                if partial.relative == Path::new(if *during { "raw" } else { "" })
                    && partial.error.kind() == io::ErrorKind::Other)
                && *callbacks == usize::from(*during)
        }),
        "{observed:?}"
    );
}

#[test]
fn conversation_upgrade_fs_source_errors_keep_reentrant_work_tentative() {
    for (kind, errno) in [
        (io::ErrorKind::Other, Some(nix::libc::EIO)),
        (io::ErrorKind::Interrupted, Some(nix::libc::EINTR)),
        (io::ErrorKind::UnexpectedEof, None),
    ] {
        for probe in [false, true] {
            for ignore in [false, true] {
                let source = tempfile::tempdir().unwrap();
                let nested = tempfile::tempdir().unwrap();
                let destination = tempfile::tempdir().unwrap();
                fs::write(source.path().join("raw"), b"data").unwrap();
                if !probe {
                    fs::write(nested.path().join("raw"), b"nested").unwrap();
                }
                let root = TrustedReadRoot::open_absolute(source.path()).unwrap();
                let nested = TrustedReadRoot::open_absolute(nested.path()).unwrap();
                let output = super::tests::publisher(destination.path(), "final");
                let fault =
                    || errno.map_or_else(|| io::Error::from(kind), io::Error::from_raw_os_error);
                let expected_kind = fault().kind();
                // Fault the actual read_source -> CheckedRead entry, not a caller Read.
                let _fault = read_fault::install_errors(if probe {
                    vec![None, Some(fault())]
                } else {
                    vec![Some(fault())]
                });
                let result = output.build(|writer| {
                    let partial = writer
                        .inventory_owned_tree(&root, |writer, _, input| {
                            let mut bytes = [0; 4];
                            if !probe && kind == io::ErrorKind::Interrupted {
                                // read_exact retries EINTR internally; the source still failed.
                                input.read_exact(&mut bytes)?;
                            } else if !probe {
                                let error = input.read(&mut [0; 1]).unwrap_err();
                                assert_eq!(error.kind(), expected_kind);
                                assert_eq!(error.raw_os_error(), errno);
                            }
                            if probe || kind != io::ErrorKind::Interrupted {
                                input.read_exact(&mut bytes)?;
                            }
                            assert_eq!(&bytes, b"data");
                            writer.add_directory(Path::new("tentative")).unwrap();
                            writer
                                .write_file(Path::new("tentative/raw"), &mut &bytes[..])
                                .unwrap();
                            assert!(
                                writer
                                    .inventory_owned_tree(&nested, |_, _, input| {
                                        io::copy(input, &mut io::sink()).map(|_| ())
                                    })
                                    .is_ok()
                            );
                            Ok(())
                        })
                        .unwrap_err();
                    assert_eq!(partial.relative, Path::new("raw"));
                    assert_eq!(partial.error.kind(), expected_kind);
                    assert_eq!(partial.error.raw_os_error(), errno);
                    assert!(matches!(
                        writer.add_directory(Path::new("later")),
                        Err(PublicationError::AlreadyFailed)
                    ));
                    if ignore { Ok(()) } else { Err(partial) }
                });
                if ignore {
                    assert!(matches!(
                        result,
                        Err(BuildError::Publication(PublicationError::AlreadyFailed))
                    ));
                } else {
                    assert!(matches!(result, Err(BuildError::Build(partial))
                        if partial.relative == Path::new("raw") && partial.error.kind() == expected_kind
                            && partial.error.raw_os_error() == errno));
                }
                assert!(!destination.path().join("final").exists());
            }
        }
    }
}

#[test]
fn conversation_upgrade_fs_eof_and_callback_error_are_distinct() {
    for mode in ["normal", "handled_overrequest", "callback_error"] {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        fs::write(source.path().join("raw"), b"data").unwrap();
        let root = TrustedReadRoot::open_absolute(source.path()).unwrap();
        let output = super::tests::publisher(destination.path(), "final");
        let result = output.build(|writer| {
            writer.inventory_owned_tree(&root, |writer, path, input| {
                let mut bytes = Vec::new();
                input.read_to_end(&mut bytes)?;
                assert_eq!(bytes, b"data");
                assert_eq!(input.read(&mut [0; 1])?, 0);
                if mode == "handled_overrequest" {
                    assert_eq!(
                        input.read_exact(&mut [0; 1]).unwrap_err().kind(),
                        io::ErrorKind::UnexpectedEof
                    );
                }
                writer.write_file(path, &mut &bytes[..]).unwrap();
                if mode == "callback_error" {
                    Err(io::ErrorKind::BrokenPipe.into())
                } else {
                    Ok(())
                }
            })
        });
        if mode == "callback_error" {
            assert!(matches!(result, Err(BuildError::Build(partial))
                if partial.relative == Path::new("raw") && partial.error.kind() == io::ErrorKind::BrokenPipe));
            assert!(!destination.path().join("final").exists());
        } else {
            result.unwrap();
            assert_eq!(
                fs::read(destination.path().join("final/raw")).unwrap(),
                b"data"
            );
        }
    }
}

#[test]
fn conversation_upgrade_fs_caught_inventory_unwind_cannot_publish() {
    for fault in [
        Some((io::ErrorKind::Other, Some(nix::libc::EIO))),
        Some((io::ErrorKind::Interrupted, Some(nix::libc::EINTR))),
        Some((io::ErrorKind::UnexpectedEof, None)),
        None,
    ] {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        fs::write(source.path().join("raw"), b"data").unwrap();
        let root = TrustedReadRoot::open_absolute(source.path()).unwrap();
        let output = super::tests::publisher(destination.path(), "final");
        let result = output.build(|writer| {
            let joined = std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        // The existing source-read seam is thread-local: install here.
                        let _fault = read_fault::install_errors(fault.map(|(kind, errno)| {
                            Some(errno.map_or_else(
                                || io::Error::from(kind),
                                io::Error::from_raw_os_error,
                            ))
                        }));
                        let _ = writer.inventory_owned_tree(&root, |writer, path, input| {
                            assert_eq!(path, Path::new("raw"));
                            writer
                                .write_file(Path::new("seal"), &mut &b"tentative"[..])
                                .unwrap();
                            if let Some((kind, errno)) = fault {
                                let error = input.read(&mut [0; 1]).unwrap_err();
                                let expected = errno.map_or_else(
                                    || io::Error::from(kind),
                                    io::Error::from_raw_os_error,
                                );
                                assert_eq!(error.kind(), expected.kind());
                                assert_eq!(error.raw_os_error(), errno);
                            } else {
                                let mut bytes = Vec::new();
                                input.read_to_end(&mut bytes).unwrap();
                                assert_eq!(bytes, b"data");
                            }
                            panic!("inventory callback incomplete");
                        });
                    })
                    .join()
            });
            // Reject setup/assertion panics rather than treating them as coverage.
            let panic = joined.unwrap_err();
            assert_eq!(
                panic.downcast_ref::<&str>(),
                Some(&"inventory callback incomplete")
            );
            Ok::<_, ()>(())
        });
        assert!(matches!(
            result,
            Err(BuildError::Publication(PublicationError::AlreadyFailed))
        ));
        assert!(!destination.path().join("final").exists());
        let temporary = fs::read_dir(destination.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::read(temporary.join("seal")).unwrap(), b"tentative");
    }
}
