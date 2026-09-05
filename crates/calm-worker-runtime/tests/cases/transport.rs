use super::*;
use std::io::Read;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;

fn connect(f: &Fixture, handle: &BoundaryHandle) -> UnixStream {
    let stream = f.runtime.connect_stdio(handle).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
}

fn response(stream: &mut BufReader<UnixStream>) -> String {
    let mut line = String::new();
    stream.read_line(&mut line).unwrap();
    line
}

#[test]
fn boundary_transport_half_closed_client_receives_delayed_response() {
    let mut f = Fixture::new();
    f.config.args.clear();
    let handle = f.prepare();
    let mut client = connect(&f, &handle);
    f.runtime.start(&handle).unwrap();
    client.write_all(b"delayed complete response\n").unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let mut reader = BufReader::new(client);
    assert_eq!(response(&mut reader), "complete response\n");
    assert_eq!(f.runtime.probe(&handle).unwrap(), BoundaryState::Running);
    assert_eq!(
        std::fs::read_to_string(f.config.workspace.join("started")).unwrap(),
        "start\n"
    );
}

#[test]
fn boundary_transport_provider_stdout_eof_keeps_process_and_stdin_alive() {
    let mut f = Fixture::new();
    f.config.args.clear(); // No background process may retain a stdout copy.
    let handle = f.prepare();
    let mut client = connect(&f, &handle);
    f.runtime.start(&handle).unwrap();
    client.write_all(b"close-stdout\n").unwrap();
    let mut reader = BufReader::new(client);
    assert_eq!(response(&mut reader), "last stdout\n");
    wait_file(&f.config.workspace.join("stdout-closed"));
    let mut byte = [0];
    assert_eq!(
        reader.read(&mut byte).unwrap(),
        0,
        "stdout EOF must reach the client"
    );
    reader.get_mut().write_all(b"still-listening\n").unwrap();
    wait_file(&f.config.workspace.join("stdin-open"));
    assert_eq!(f.runtime.probe(&handle).unwrap(), BoundaryState::Running);
}

#[test]
fn boundary_transport_full_disconnect_reconnect_and_second_client_do_not_interleave() {
    let mut f = Fixture::new();
    f.config.args.clear();
    let handle = f.prepare();
    let mut first = BufReader::new(connect(&f, &handle));
    f.runtime.start(&handle).unwrap();
    first.get_mut().write_all(b"first\n").unwrap();
    assert_eq!(response(&mut first), "first\n");

    let mut second = connect(&f, &handle);
    let _ = second.write_all(b"forbidden second client\n");
    let mut byte = [0];
    match second.read(&mut byte) {
        Ok(0) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        result => panic!("second client was not rejected: {result:?}"),
    }
    first.get_mut().write_all(b"still first\n").unwrap();
    assert_eq!(response(&mut first), "still first\n");
    drop(first);
    let mut resumed = BufReader::new(connect(&f, &handle));
    resumed.get_mut().write_all(b"resumed\n").unwrap();
    assert_eq!(response(&mut resumed), "resumed\n");
    assert_eq!(
        std::fs::read_to_string(f.config.workspace.join("requests")).unwrap(),
        "first\nstill first\nresumed\n"
    );
    assert_eq!(
        std::fs::read_to_string(f.config.workspace.join("started")).unwrap(),
        "start\n"
    );
    assert_eq!(f.runtime.probe(&handle).unwrap(), BoundaryState::Running);
}

#[test]
fn boundary_transport_disconnected_client_does_not_discard_delayed_stdout() {
    let mut f = Fixture::new();
    f.config.args.clear();
    let handle = f.prepare();
    let mut client = connect(&f, &handle);
    f.runtime.start(&handle).unwrap();
    client.write_all(b"delayed retained response\n").unwrap();
    wait_file(&f.config.workspace.join("requests"));
    drop(client);
    // The old socket is gone before the fake provider emits its delayed line.
    std::thread::sleep(Duration::from_millis(250));
    let mut replacement = BufReader::new(connect(&f, &handle));
    assert_eq!(response(&mut replacement), "retained response\n");
    replacement.get_mut().write_all(b"new request\n").unwrap();
    assert_eq!(response(&mut replacement), "new request\n");
    assert_eq!(
        std::fs::read_to_string(f.config.workspace.join("started")).unwrap(),
        "start\n"
    );
}
