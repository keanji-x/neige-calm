use super::*;
use crate::offline_audit::{PartialRead, RawEntry, inventory_owned_tree};

#[derive(Debug)]
pub enum BuildError<E> {
    Build(E),
    Publication(PublicationError),
}

/// A borrowed provisional sink, with no publication or ownership-transfer door.
/// Only Publisher::build can obtain one; no completion capability escapes failure.
pub struct BuildWriter<'a> {
    publisher: &'a mut Publisher,
}

// An inventory owns provisional output until the whole scan returns Ok.
struct InventoryCompletion<'a, 'p> {
    writer: &'a mut BuildWriter<'p>,
    complete: bool,
}

impl Drop for InventoryCompletion<'_, '_> {
    fn drop(&mut self) {
        if !self.complete {
            self.writer.publisher.failed = true;
        }
    }
}

impl Publisher {
    /// Consume this publisher, build every owned file, then publish only on Ok.
    /// The closure can stream raw evidence and write additional caller-owned files
    /// in one transaction. Errors preserve the incomplete tree for diagnosis.
    /// Success establishes filesystem durability only, not audit eligibility.
    pub fn build<T, E>(
        mut self,
        build: impl FnOnce(&mut BuildWriter<'_>) -> Result<T, E>,
    ) -> Result<(T, PublishedDurable), BuildError<E>> {
        let value = build(&mut BuildWriter {
            publisher: &mut self,
        })
        .map_err(BuildError::Build)?;
        let published = self.finish().map_err(BuildError::Publication)?;
        Ok((value, published))
    }
}

impl BuildWriter<'_> {
    fn ensure_healthy(&self) -> io::Result<()> {
        if self.publisher.failed {
            Err(io::Error::other("build writer already failed"))
        } else {
            Ok(())
        }
    }

    /// Declare a new directory below an already declared parent.
    pub fn add_directory(&mut self, path: &Path) -> Result<(), PublicationError> {
        self.publisher.add_directory(path)
    }

    /// Create, stream and sync one fresh 0600 file. All errors latch failure.
    pub fn write_file(
        &mut self,
        path: &Path,
        input: &mut (impl Read + ?Sized),
    ) -> Result<(), PublicationError> {
        self.publisher.write_file(path, input)
    }

    /// Inventory with a provisional sink, retaining the actual source FD for
    /// overlap checks. All inventory errors latch failure, even if ignored by
    /// the build closure, including errors outside the bytes callback.
    /// An inventory that unwinds before completing also invalidates the owner,
    /// even if the caller catches the panic and returns Ok from build.
    /// Before the enclosing inventory returns, successful writes, directory adds
    /// and nested inventories are tentative; they do not commit any output.
    /// A source read error is retained by the reader, even if the callback recovers
    /// or retries Interrupted. The outer inventory returns PartialRead with the
    /// first source cause/path and latches this owner; build cannot then publish.
    /// Publisher operation errors still latch immediately. An explicit callback
    /// error is preserved when no earlier source error takes precedence.
    /// Normal EOF is not a source error. A caller's oversized read_exact may
    /// create UnexpectedEof; handling that locally does not invalidate the source.
    /// The caller still supplies independent frozen-source and ownership proof.
    pub fn inventory_owned_tree(
        &mut self,
        root: &TrustedReadRoot,
        mut bytes: impl FnMut(&mut Self, &Path, &mut dyn Read) -> io::Result<()>,
    ) -> Result<Vec<RawEntry>, PartialRead> {
        let mut completion = InventoryCompletion {
            writer: self,
            complete: false,
        };
        completion
            .writer
            .ensure_healthy()
            .map_err(|error| PartialRead {
                relative: PathBuf::new(),
                error,
            })?;
        completion
            .writer
            .publisher
            .watch_source(root)
            .map_err(|error| PartialRead {
                relative: PathBuf::new(),
                error,
            })?;
        let result = inventory_owned_tree(root, |path, input| {
            completion.writer.ensure_healthy()?;
            bytes(completion.writer, path, input)?;
            completion.writer.ensure_healthy()
        });
        // Caller-owned callback state must also drop before disarming the lease.
        drop(bytes);
        completion.complete = result.is_ok();
        result
    }
}
