#![cfg(target_os = "linux")]

use memmap2::{Mmap, MmapMut};
use std::{
    env,
    ffi::{OsStr, OsString},
    fs::{File, Metadata, OpenOptions},
    io,
    mem::{size_of, MaybeUninit},
    ops::Range,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    path::{Path, PathBuf},
    ptr,
};

//files with utilities:
use crate::error::{ pthread_error, invalid_input, invalid_data};
mod error;

const STATE_MAGIC: u64 = 0x444F_4D41_494E_4D4D; 
const STATE_VERSION: u32 = 1;
const SEMAPHORE_SLOTS: u32 = 4;

pub struct DomainFile {
    path: PathBuf,
    mmap: Mmap,
    identity: InputIdentity,
}


pub struct OptionsFile {
    path: PathBuf,
    mmap: Option<Mmap>,
    len: usize,
}


#[derive(Debug, Clone)]
pub struct DomainClaim {
    pub number: u64,
    pub range: Range<usize>,
}

#[derive(Debug, Clone, Copy)]
struct InputIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}


#[repr(C)]
struct SharedStateHeader {
    magic: u64,
    version: u32,
    header_size: u32,

    input_device: u64,
    input_inode: u64,
    input_size: u64,
    input_modified_seconds: i64,
    input_modified_nanoseconds: i64,

    next_offset: u64,
    claimed_domains: u64,
    semaphore_slots: u32,
    _reserved: u32,

    mutex: libc::pthread_mutex_t,
    semaphore: libc::sem_t,
}


pub struct SharedCoordinator {
    mmap: MmapMut,
    path: PathBuf,
}

struct FileLock<'a> {
    file: &'a File,
}

struct SemaphoreGuard {
    semaphore: *mut libc::sem_t,
}

struct MutexGuard {
    mutex: *mut libc::pthread_mutex_t,
}

impl DomainFile {
    pub fn open(path: &Path) -> io::Result<Self> {
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

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.mmap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.mmap.is_empty()
    }

    pub fn bytes(&self) -> &[u8] {
        &self.mmap
    }


    pub fn domain_bytes(&self, claim: &DomainClaim) -> &[u8] {
        &self.mmap[claim.range.clone()]
    }
}

impl OptionsFile {
    pub fn open(path: &Path) -> io::Result<Self> {
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

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes(&self) -> &[u8] {
        self.mmap.as_deref().unwrap_or(&[])
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
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

impl SharedCoordinator {

    pub fn open_or_create(path: &Path, input: &DomainFile) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;

        // Prevent two processes from initializing pthread objects at once.
        let _initialization_lock = FileLock::exclusive(&file)?;

        if file.metadata()?.len() == 0 {
            initialize_state_file(&file, input.identity)?;
        }

        if file.metadata()?.len() != size_of::<SharedStateHeader>() as u64 {
            return Err(invalid_data(
                "the state file has the wrong size; delete it and start again",
            ));
        }

        let mmap = unsafe { MmapMut::map_mut(&file)? };
        let coordinator = Self {
            mmap,
            path: path.to_path_buf(),
        };
        coordinator.validate(input.identity)?;
        Ok(coordinator)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn with_worker_slot<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> io::Result<T>,
    ) -> io::Result<T> {
        let semaphore = self.semaphore_ptr();
        sem_wait_retry(semaphore)?;
        let _guard = SemaphoreGuard { semaphore };
        operation(self)
    }

  
    pub fn claim_next_domain(&mut self, input: &DomainFile) -> io::Result<Option<DomainClaim>> {
        let _guard = self.lock_state()?;
        let header = self.header_ptr_mut();
        let bytes = input.bytes();

        let mut offset = usize::try_from(unsafe {
            ptr::read(ptr::addr_of!((*header).next_offset))
        })
        .map_err(|_| invalid_data("next_offset does not fit usize"))?;

        if offset > bytes.len() {
            return Err(invalid_data("next_offset is beyond the input mmap"));
        }

        loop {
          
            while offset < bytes.len()
                && matches!(bytes[offset], b'\n' | b'\r' | b' ' | b'\t')
            {
                offset += 1;
            }

            if offset >= bytes.len() {
                unsafe {
                    ptr::write(ptr::addr_of_mut!((*header).next_offset), bytes.len() as u64);
                }
                return Ok(None);
            }

            let mut line_end = offset;
            while line_end < bytes.len() && bytes[line_end] != b'\n' {
                line_end += 1;
            }

            let next_offset = if line_end < bytes.len() {
                line_end + 1
            } else {
                line_end
            };

            let mut trimmed_end = line_end;
            while trimmed_end > offset
                && matches!(bytes[trimmed_end - 1], b'\r' | b' ' | b'\t')
            {
                trimmed_end -= 1;
            }

            unsafe {
                ptr::write(
                    ptr::addr_of_mut!((*header).next_offset),
                    next_offset as u64,
                );
            }

            if trimmed_end == offset {
                offset = next_offset;
                continue;
            }

            let number = unsafe {
                let current = ptr::read(ptr::addr_of!((*header).claimed_domains));
                let next = current
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("claimed domain counter overflow"))?;
                ptr::write(ptr::addr_of_mut!((*header).claimed_domains), next);
                next
            };

            return Ok(Some(DomainClaim {
                number,
                range: offset..trimmed_end,
            }));
        }
    }

    /// Current shared cursor, useful later for diagnostics or progress reports.
    pub fn progress(&mut self) -> io::Result<(u64, u64)> {
        let _guard = self.lock_state()?;
        let header = self.header_ptr_const();
        let next_offset = unsafe { ptr::read(ptr::addr_of!((*header).next_offset)) };
        let claimed_domains = unsafe { ptr::read(ptr::addr_of!((*header).claimed_domains)) };
        Ok((next_offset, claimed_domains))
    }

    /// Persists the sidecar state to disk. Shared-memory visibility between
    /// processes does not require a flush, but persistence across a crash may.
    pub fn flush(&self) -> io::Result<()> {
        self.mmap.flush()
    }

    fn lock_state(&mut self) -> io::Result<MutexGuard> {
        let mutex = self.mutex_ptr();
        let rc = unsafe { libc::pthread_mutex_lock(mutex) };

        if rc == libc::EOWNERDEAD {
            let consistent_rc = unsafe { libc::pthread_mutex_consistent(mutex) };
            if consistent_rc != 0 {
                unsafe {
                    libc::pthread_mutex_unlock(mutex);
                }
                return Err(pthread_error(
                    "pthread_mutex_consistent",
                    consistent_rc,
                ));
            }
        } else if rc != 0 {
            return Err(pthread_error("pthread_mutex_lock", rc));
        }

        Ok(MutexGuard { mutex })
    }

    fn header_ptr_const(&self) -> *const SharedStateHeader {
        self.mmap.as_ptr().cast::<SharedStateHeader>()
    }

    fn header_ptr_mut(&mut self) -> *mut SharedStateHeader {
        self.mmap.as_mut_ptr().cast::<SharedStateHeader>()
    }

    fn mutex_ptr(&mut self) -> *mut libc::pthread_mutex_t {
        let header = self.header_ptr_mut();
        unsafe { ptr::addr_of_mut!((*header).mutex) }
    }

    fn semaphore_ptr(&mut self) -> *mut libc::sem_t {
        let header = self.header_ptr_mut();
        unsafe { ptr::addr_of_mut!((*header).semaphore) }
    }

    fn validate(&self, input: InputIdentity) -> io::Result<()> {
        let header = self.header_ptr_const();
        let (
            magic,
            version,
            header_size,
            input_device,
            input_inode,
            input_size,
            modified_seconds,
            modified_nanoseconds,
            next_offset,
        ) = unsafe {
            (
                ptr::read(ptr::addr_of!((*header).magic)),
                ptr::read(ptr::addr_of!((*header).version)),
                ptr::read(ptr::addr_of!((*header).header_size)),
                ptr::read(ptr::addr_of!((*header).input_device)),
                ptr::read(ptr::addr_of!((*header).input_inode)),
                ptr::read(ptr::addr_of!((*header).input_size)),
                ptr::read(ptr::addr_of!((*header).input_modified_seconds)),
                ptr::read(ptr::addr_of!((*header).input_modified_nanoseconds)),
                ptr::read(ptr::addr_of!((*header).next_offset)),
            )
        };

        if magic != STATE_MAGIC {
            return Err(invalid_data(
                "the state file is not initialized or belongs to another program",
            ));
        }
        if version != STATE_VERSION {
            return Err(invalid_data("unsupported state-file version"));
        }
        if header_size as usize != size_of::<SharedStateHeader>() {
            return Err(invalid_data(
                "SharedStateHeader size does not match this build",
            ));
        }

        let same_input = input_device == input.device
            && input_inode == input.inode
            && input_size == input.size
            && modified_seconds == input.modified_seconds
            && modified_nanoseconds == input.modified_nanoseconds;

        if !same_input {
            return Err(invalid_data(
                "the state file belongs to a different or modified input file; delete the state file",
            ));
        }

        if next_offset > input.size {
            return Err(invalid_data("state cursor is beyond the input file"));
        }

        Ok(())
    }
}

impl<'a> FileLock<'a> {
    fn exclusive(file: &'a File) -> io::Result<Self> {
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(FileLock { file })
    }
}

impl Drop for FileLock<'_> {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl Drop for SemaphoreGuard {
    fn drop(&mut self) {
        unsafe {
            libc::sem_post(self.semaphore);
        }
    }
}

impl Drop for MutexGuard {
    fn drop(&mut self) {
        unsafe {
            libc::pthread_mutex_unlock(self.mutex);
        }
    }
}

fn initialize_state_file(file: &File, input: InputIdentity) -> io::Result<()> {
    file.set_len(size_of::<SharedStateHeader>() as u64)?;

    let mut mmap = unsafe { MmapMut::map_mut(file)? };
    mmap.fill(0);

    let header = mmap.as_mut_ptr().cast::<SharedStateHeader>();
    let mutex = unsafe { ptr::addr_of_mut!((*header).mutex) };
    let semaphore = unsafe { ptr::addr_of_mut!((*header).semaphore) };

    let mut attributes = MaybeUninit::<libc::pthread_mutexattr_t>::uninit();
    let rc = unsafe { libc::pthread_mutexattr_init(attributes.as_mut_ptr()) };
    if rc != 0 {
        return Err(pthread_error("pthread_mutexattr_init", rc));
    }
    let mut attributes = unsafe { attributes.assume_init() };

    let initialization_result = (|| -> io::Result<()> {
        let rc = unsafe {
            libc::pthread_mutexattr_setpshared(
                &mut attributes,
                libc::PTHREAD_PROCESS_SHARED,
            )
        };
        if rc != 0 {
            return Err(pthread_error("pthread_mutexattr_setpshared", rc));
        }

        let rc = unsafe {
            libc::pthread_mutexattr_setrobust(&mut attributes, libc::PTHREAD_MUTEX_ROBUST)
        };
        if rc != 0 {
            return Err(pthread_error("pthread_mutexattr_setrobust", rc));
        }

        let rc = unsafe { libc::pthread_mutex_init(mutex, &attributes) };
        if rc != 0 {
            return Err(pthread_error("pthread_mutex_init", rc));
        }

        if unsafe { libc::sem_init(semaphore, 1, SEMAPHORE_SLOTS) } != 0 {
            let error = io::Error::last_os_error();
            unsafe {
                libc::pthread_mutex_destroy(mutex);
            }
            return Err(io::Error::new(
                error.kind(),
                format!("sem_init failed: {error}"),
            ));
        }

        unsafe {
            ptr::write(
                ptr::addr_of_mut!((*header).version),
                STATE_VERSION,
            );
            ptr::write(
                ptr::addr_of_mut!((*header).header_size),
                size_of::<SharedStateHeader>() as u32,
            );
            ptr::write(
                ptr::addr_of_mut!((*header).input_device),
                input.device,
            );
            ptr::write(ptr::addr_of_mut!((*header).input_inode), input.inode);
            ptr::write(ptr::addr_of_mut!((*header).input_size), input.size);
            ptr::write(
                ptr::addr_of_mut!((*header).input_modified_seconds),
                input.modified_seconds,
            );
            ptr::write(
                ptr::addr_of_mut!((*header).input_modified_nanoseconds),
                input.modified_nanoseconds,
            );
            ptr::write(ptr::addr_of_mut!((*header).next_offset), 0);
            ptr::write(ptr::addr_of_mut!((*header).claimed_domains), 0);
            ptr::write(
                ptr::addr_of_mut!((*header).semaphore_slots),
                SEMAPHORE_SLOTS,
            );

            // Written last so another process never accepts a partial header.
            ptr::write(ptr::addr_of_mut!((*header).magic), STATE_MAGIC);
        }

        mmap.flush()?;
        Ok(())
    })();

    let destroy_rc = unsafe { libc::pthread_mutexattr_destroy(&mut attributes) };
    if initialization_result.is_ok() && destroy_rc != 0 {
        return Err(pthread_error("pthread_mutexattr_destroy", destroy_rc));
    }

    initialization_result
}

fn sem_wait_retry(semaphore: *mut libc::sem_t) -> io::Result<()> {
    loop {
        if unsafe { libc::sem_wait(semaphore) } == 0 {
            return Ok(());
        }

        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::new(
                error.kind(),
                format!("sem_wait failed: {error}"),
            ));
        }
    }
}

#[derive(Debug)]
struct Arguments {
    input_path: PathBuf,
    options_path: Option<PathBuf>,
    state_path: PathBuf,
}

fn parse_arguments() -> io::Result<Arguments> {
    let mut arguments = env::args_os();
    let program = arguments
        .next()
        .unwrap_or_else(|| OsString::from("rust_mmap_sync"));

    let input_path = match arguments.next() {
        Some(value)
            if value.as_os_str() != OsStr::new("-h")
                && value.as_os_str() != OsStr::new("--help") =>
        {
            PathBuf::from(value)
        }
        _ => {
            print_usage(&program);
            return Err(invalid_input("missing required domain input file"));
        }
    };

    let mut options_path = None;
    let mut state_path = None;

    while let Some(argument) = arguments.next() {
        if argument.as_os_str() == OsStr::new("-o")
            || argument.as_os_str() == OsStr::new("--options")
        {
            let value = arguments
                .next()
                .ok_or_else(|| invalid_input("missing path after --options"))?;
            options_path = Some(PathBuf::from(value));
        } else if argument.as_os_str() == OsStr::new("--state") {
            let value = arguments
                .next()
                .ok_or_else(|| invalid_input("missing path after --state"))?;
            state_path = Some(PathBuf::from(value));
        } else if argument.as_os_str() == OsStr::new("-h")
            || argument.as_os_str() == OsStr::new("--help")
        {
            print_usage(&program);
            std::process::exit(0);
        } else {
            print_usage(&program);
            return Err(invalid_input(format!(
                "unknown argument: {}",
                argument.to_string_lossy()
            )));
        }
    }

    let state_path = state_path.unwrap_or_else(|| default_state_path(&input_path));

    Ok(Arguments {
        input_path,
        options_path,
        state_path,
    })
}

fn default_state_path(input_path: &Path) -> PathBuf {
    let mut value = input_path.as_os_str().to_os_string();
    value.push(".state");
    PathBuf::from(value)
}

fn print_usage(program: &OsString) {
    eprintln!(
        "Usage:\n  {} <domains-file> [--options <options-file>] [--state <state-file>]",
        Path::new(program.as_os_str()).display()
    );
}




fn run() -> io::Result<()> {
    let arguments = parse_arguments()?;

    let domains = DomainFile::open(&arguments.input_path)?;

    
    let options = arguments
        .options_path
        .as_deref()
        .map(OptionsFile::open)
        .transpose()?;

    
    let coordinator = SharedCoordinator::open_or_create(&arguments.state_path, &domains)?;

    println!(
        "Mapped domain input: {} ({} bytes)",
        domains.path().display(),
        domains.len()
    );
    println!("Shared state: {}", coordinator.path().display());

    if let Some(options) = &options {
        println!(
            "Mapped optional options file: {} ({} bytes; not interpreted yet)",
            options.path().display(),
            options.len()
        );
    }

    println!("Ready. No domains are processed by this base program.");


    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}