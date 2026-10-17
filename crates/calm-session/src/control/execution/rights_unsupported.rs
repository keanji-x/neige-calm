//! Registered descriptor transfer requires the qualified Linux transport.
use serde::{Serialize, de::DeserializeOwned};
use std::{
    io,
    os::fd::{BorrowedFd, OwnedFd},
};
use tokio::net::UnixStream;

pub async fn send<T: Serialize>(_: &UnixStream, _: &T, _: &[BorrowedFd<'_>]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "registered executor transport requires Linux",
    ))
}
pub async fn receive<T: DeserializeOwned>(_: &UnixStream) -> io::Result<(T, Vec<OwnedFd>)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "registered executor transport requires Linux",
    ))
}
