//! Where an import reads its file from, and how `apply_import` knows it
//! is still the file `plan_import` checked (ADR 0053).

use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::Arc;
use std::time::SystemTime;

use super::ImportProblem;
use crate::error::{Error, Result};

/// The file an import reads, opened again for each pass.
#[derive(Debug, Clone)]
pub(super) enum Source {
    /// A file chosen by the user.
    File(PathBuf),
    /// Archive bytes in memory, for tests.
    #[cfg(test)]
    Bytes(Arc<[u8]>),
}

/// What identifies a file on disk between the plan and the apply: its
/// size, when it was last changed, and which file it is. The hash of
/// `cards.json` covers the content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    device: u64,
    inode: u64,
}

/// An opened source.
#[derive(Debug)]
pub(super) enum Input {
    File(BufReader<File>),
    #[cfg(test)]
    Bytes(io::Cursor<Arc<[u8]>>),
}

impl Read for Input {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::File(f) => f.read(buf),
            #[cfg(test)]
            Self::Bytes(b) => b.read(buf),
        }
    }
}

impl Seek for Input {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            Self::File(f) => f.seek(pos),
            #[cfg(test)]
            Self::Bytes(b) => b.seek(pos),
        }
    }
}

/// An opened source with its length and, for a file, its stamp.
#[derive(Debug)]
pub(super) struct Opened {
    pub(super) input: Input,
    pub(super) len: u64,
    pub(super) stamp: Option<Stamp>,
}

impl Source {
    /// Opens the source for reading. Only a regular file is opened:
    /// opening a FIFO blocks until something writes to it (the import
    /// would never end), and a device such as /dev/zero never ends.
    /// Checked before opening, through symbolic links, and again on the
    /// open file. A FIFO swapped in between the two checks would still
    /// block; the path comes from the user's own file chooser, so that
    /// race is accepted (ADR 0025).
    pub(super) fn open(&self) -> Result<Opened> {
        match self {
            Self::File(path) => {
                let io_err = |source| Error::Io {
                    path: path.clone(),
                    source,
                };
                let not_a_file = || Error::ImportRefused(ImportProblem::NotAFile);
                if !fs::metadata(path).map_err(io_err)?.is_file() {
                    return Err(not_a_file());
                }
                let file = File::open(path).map_err(io_err)?;
                let metadata = file.metadata().map_err(io_err)?;
                if !metadata.is_file() {
                    return Err(not_a_file());
                }
                let stamp = Stamp {
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                };
                Ok(Opened {
                    input: Input::File(BufReader::with_capacity(64 << 10, file)),
                    len: stamp.len,
                    stamp: Some(stamp),
                })
            }
            #[cfg(test)]
            Self::Bytes(bytes) => Ok(Opened {
                len: bytes.len() as u64,
                input: Input::Bytes(io::Cursor::new(Arc::clone(bytes))),
                stamp: None,
            }),
        }
    }
}
