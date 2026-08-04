use crate::error::pthread_error;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

/// Holds an exclusive `flock` on a file, released on drop.
pub(crate) struct FileLock<'a> {
    file: &'a File,
}

impl<'a> FileLock<'a> {
    pub(crate) fn exclusive(file: &'a File) -> io::Result<Self> {
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

/// Holds one slot of a POSIX semaphore, posted back on drop.
pub(crate) struct SemaphoreGuard {
    semaphore: *mut libc::sem_t,
}

impl SemaphoreGuard {
    /// # Safety
    /// `semaphore` must point to a live, initialised `sem_t` that outlives the guard.
    pub(crate) unsafe fn acquire(semaphore: *mut libc::sem_t) -> io::Result<Self> {
        unsafe { sem_wait_retry(semaphore)? };
        Ok(SemaphoreGuard { semaphore })
    }
}

impl Drop for SemaphoreGuard {
    fn drop(&mut self) {
        unsafe {
            libc::sem_post(self.semaphore);
        }
    }
}

/// Holds a process-shared pthread mutex, unlocked on drop.
pub(crate) struct MutexGuard {
    mutex: *mut libc::pthread_mutex_t,
}

impl MutexGuard {
    /// # Safety
    /// `mutex` must point to a live, initialised, process-shared mutex that
    /// outlives the guard.
    pub(crate) unsafe fn lock(mutex: *mut libc::pthread_mutex_t) -> io::Result<Self> {
        let rc = unsafe { libc::pthread_mutex_lock(mutex) };

        if rc == libc::EOWNERDEAD {
            // Previous owner died holding the lock. Mark the state consistent
            // so the mutex stays usable instead of deadlocking forever.
            let consistent_rc = unsafe { libc::pthread_mutex_consistent(mutex) };
            if consistent_rc != 0 {
                unsafe { libc::pthread_mutex_unlock(mutex) };
                return Err(pthread_error("pthread_mutex_consistent", consistent_rc));
            }
        } else if rc != 0 {
            return Err(pthread_error("pthread_mutex_lock", rc));
        }

        Ok(MutexGuard { mutex })
    }
}

impl Drop for MutexGuard {
    fn drop(&mut self) {
        unsafe {
            libc::pthread_mutex_unlock(self.mutex);
        }
    }
}

/// `sem_wait` that retries when interrupted by a signal.
///
/// # Safety
/// `semaphore` must point to a live, initialised `sem_t`.
unsafe fn sem_wait_retry(semaphore: *mut libc::sem_t) -> io::Result<()> {
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