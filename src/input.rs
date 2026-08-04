use crate::error::invalid_input;
use memmap2::Mmap;
use std::fs::{File, Metadata};
use std::io;
use std::ops::Range;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Fingerprint of the input file, stored in the shared state so a stale
/// cursor is never applied to a different or modified file.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InputIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) size: u64,
    pub(crate) modified_seconds: i64,
    pub(crate) modified_nanoseconds: i64,
}

impl InputIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        }
    }
}

/// One domain handed out to a worker, as a byte range into the mapped input.
#[derive(Debug, Clone)]
pub(crate) struct DomainClaim {
    pub(crate) number: u64,
    pub(crate) range: Range<usize>,
}

/// The list of domains, memory-mapped read-only.
pub(crate) struct DomainFile {
    path: PathBuf,
    mmap: Mmap,
    identity: InputIdentity,
}

impl DomainFile {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;

        if metadata.len() == 0 {
            return Err(invalid_input("the domain input file is empty"));
        }

        let mmap = unsafe { Mmap::map(&file)? };
        let identity = InputIdentity::from_metadata(&metadata);

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            identity,
        })
    }

    pub(crate) fn identity(&self) -> InputIdentity {
        self.identity
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn len(&self) -> usize {
        self.mmap.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.mmap.is_empty()
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.mmap
    }

    pub(crate) fn domain_bytes(&self, claim: &DomainClaim) -> &[u8] {
        &self.mmap[claim.range.clone()]
    }
}

/// Optional second input file, mapped the same way. Empty files cannot be
/// mapped, so the mapping is absent rather than zero-length.
pub(crate) struct OptionsFile {
    path: PathBuf,
    mmap: Option<Mmap>,
    len: usize,
}

impl OptionsFile {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = usize::try_from(file.metadata()?.len())
            .map_err(|_| invalid_input("options file is too large for this platform"))?;

        let mmap = if len == 0 {
            None
        } else {
            Some(unsafe { Mmap::map(&file)? })
        };

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            len,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        self.mmap.as_deref().unwrap_or(&[])
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }
}