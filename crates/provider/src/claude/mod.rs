//! Claude stream-json protocol and provider event translation.

#[cfg(test)]
mod compact_tests;
pub mod discovery;
#[cfg(test)]
mod discovery_tests;
pub mod line_writer;
pub mod permission;
pub mod protocol;
#[cfg(test)]
mod result_tests;
#[cfg(test)]
mod stream_tests;
#[cfg(test)]
mod tests;
pub mod translate;
