/// Observe the CLI argv inside the workspace helper's declared shell target.
pub fn shell_argv(command_line: &str) -> Vec<String> {
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("set -- {command_line}; printf '%s\\0' \"$@\""))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let mut argv: Vec<String> = String::from_utf8(output.stdout)
        .unwrap()
        .split_terminator('\0')
        .map(ToOwned::to_owned)
        .collect();
    if argv.first().is_some_and(|arg| arg == "exec") {
        argv.remove(0);
    }
    if argv
        .get(1)
        .is_some_and(|arg| arg == "--workspace-environment-exec")
    {
        let boundary = argv.iter().position(|arg| arg == "--").unwrap();
        let target = &argv[boundary + 1..];
        assert_eq!(target.len(), 3, "helper shell target: {target:?}");
        assert_eq!(&target[..2], ["/bin/sh", "-c"]);
        return shell_argv(&target[2]);
    }
    argv
}
