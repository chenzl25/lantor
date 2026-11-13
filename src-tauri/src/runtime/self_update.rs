//! A supervisor replaces itself with a newer build of its own executable once
//! no agent is working, so a rebuilt binary takes over without a manual
//! restart and without cutting a turn short.
//!
//! The supervisor notes its executable file when it starts and looks at the
//! file again on every maintenance pass. A different file that is unchanged on
//! the next pass is a newer build waiting to take over; a file still being
//! written changes between passes. While one waits, the supervisor checks
//! whether anything is working before it schedules more work. When nothing is,
//! it stops its idle warm processes and executes the new file in place, with
//! the same process id, arguments and environment. The supervisor lock is
//! close-on-exec, so the exec releases it and the new build takes it again as
//! it starts.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// An executable file as the supervisor saw it. A rebuild writes a new file
/// (cargo links a new inode in place), and an edit in place changes its size
/// or modification time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ExecutableFile {
    device: u64,
    inode: u64,
    len: u64,
    modified: SystemTime,
}

impl ExecutableFile {
    pub(crate) fn read(path: &Path) -> io::Result<Self> {
        let metadata = fs::metadata(path)?;
        #[cfg(unix)]
        let (device, inode) = (metadata.dev(), metadata.ino());
        #[cfg(not(unix))]
        let (device, inode) = (0, 0);
        Ok(Self {
            device,
            inode,
            len: metadata.len(),
            modified: metadata.modified()?,
        })
    }

    pub(crate) fn modified(&self) -> SystemTime {
        self.modified
    }
}

/// What the supervisor knows about newer builds of its executable.
pub(crate) struct SelfUpdate {
    path: PathBuf,
    running: ExecutableFile,
    /// A different file seen on the last pass, which may still be written.
    candidate: Option<ExecutableFile>,
    /// A newer build that takes over once nothing is working.
    pending: Option<ExecutableFile>,
    /// A build that could not be executed. It is not tried again; a later
    /// build replaces it.
    failed: Option<ExecutableFile>,
}

impl SelfUpdate {
    /// Starts from the file this process runs. `None` when the executable
    /// cannot be found or read, which leaves the supervisor as it is.
    pub(crate) fn for_current_executable() -> Option<Self> {
        let path = std::env::current_exe().ok()?;
        let running = ExecutableFile::read(&path).ok()?;
        Some(Self::new(path, running))
    }

    pub(crate) fn new(path: PathBuf, running: ExecutableFile) -> Self {
        Self {
            path,
            running,
            candidate: None,
            pending: None,
            failed: None,
        }
    }

    pub(crate) fn running(&self) -> &ExecutableFile {
        &self.running
    }

    pub(crate) fn pending(&self) -> Option<&ExecutableFile> {
        self.pending.as_ref()
    }

    /// Looks at the executable file again. Returns whether the pending build
    /// changed, appearing, being replaced or going away.
    pub(crate) fn check(&mut self) -> bool {
        let current = ExecutableFile::read(&self.path).ok();
        self.observe(current)
    }

    /// Whether the file on disk is still the pending build.
    pub(crate) fn pending_is_current(&self) -> bool {
        self.pending.is_some() && ExecutableFile::read(&self.path).ok() == self.pending
    }

    pub(crate) fn observe(&mut self, current: Option<ExecutableFile>) -> bool {
        let before = self.pending;
        match current {
            None => {
                self.candidate = None;
                self.pending = None;
            }
            Some(file) if file == self.running || Some(file) == self.failed => {
                self.candidate = None;
                self.pending = None;
            }
            Some(file) if Some(file) == self.pending => {}
            Some(file) if Some(file) == self.candidate => {
                self.candidate = None;
                self.pending = Some(file);
            }
            Some(file) => {
                self.candidate = Some(file);
                self.pending = None;
            }
        }
        self.pending != before
    }

    /// Executes the pending build in place of this process. Returns only if
    /// that failed; the build is then not tried again.
    pub(crate) fn exec_pending(&mut self) -> io::Error {
        let error = exec_in_place(&self.path);
        self.failed = self.pending.take();
        error
    }
}

#[cfg(unix)]
fn exec_in_place(path: &Path) -> io::Error {
    use std::os::unix::process::CommandExt;

    std::process::Command::new(path)
        .args(std::env::args_os().skip(1))
        .exec()
}

#[cfg(not(unix))]
fn exec_in_place(_path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "replacing the supervisor in place needs a Unix exec",
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::{ExecutableFile, SelfUpdate};

    fn file(inode: u64, seconds: u64) -> ExecutableFile {
        ExecutableFile {
            device: 1,
            inode,
            len: 100,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
        }
    }

    fn update() -> SelfUpdate {
        SelfUpdate::new("/lantor".into(), file(1, 10))
    }

    #[test]
    fn a_new_file_waits_until_it_is_unchanged_on_the_next_pass() {
        let mut update = update();
        assert!(!update.observe(Some(file(1, 10))));
        assert_eq!(update.pending(), None);

        assert!(!update.observe(Some(file(2, 20))));
        assert_eq!(update.pending(), None, "it may still be written");
        assert!(update.observe(Some(file(2, 20))));
        assert_eq!(update.pending(), Some(&file(2, 20)));
        assert!(!update.observe(Some(file(2, 20))));
    }

    #[test]
    fn a_file_that_changes_again_starts_over() {
        let mut update = update();
        update.observe(Some(file(2, 20)));
        update.observe(Some(file(2, 20)));

        // Rebuilt again before it could take over.
        assert!(update.observe(Some(file(3, 30))));
        assert_eq!(update.pending(), None);
        assert!(update.observe(Some(file(3, 30))));
        assert_eq!(update.pending(), Some(&file(3, 30)));

        // Written in place: a new size is a new file.
        let mut grown = file(3, 30);
        grown.len = 200;
        update.observe(Some(grown));
        assert_eq!(update.pending(), None);
    }

    #[test]
    fn nothing_waits_when_the_running_build_is_back_or_the_file_is_gone() {
        let mut update = update();
        update.observe(Some(file(2, 20)));
        update.observe(Some(file(2, 20)));
        assert!(update.observe(Some(file(1, 10))));
        assert_eq!(update.pending(), None);

        update.observe(Some(file(2, 20)));
        update.observe(Some(file(2, 20)));
        assert!(update.observe(None));
        assert_eq!(update.pending(), None);
    }

    #[test]
    fn a_build_that_failed_to_start_is_not_tried_again() {
        let mut update = SelfUpdate::new("/nonexistent/lantor".into(), file(1, 10));
        update.observe(Some(file(2, 20)));
        update.observe(Some(file(2, 20)));

        let error = update.exec_pending();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(update.pending(), None);
        update.observe(Some(file(2, 20)));
        update.observe(Some(file(2, 20)));
        assert_eq!(update.pending(), None);

        // The next build is tried.
        update.observe(Some(file(3, 30)));
        update.observe(Some(file(3, 30)));
        assert_eq!(update.pending(), Some(&file(3, 30)));
    }
}
