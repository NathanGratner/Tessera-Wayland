//! Noticing changes to the scripts folder.
//!
//! An inotify descriptor is just another file descriptor, so it sits in the
//! calloop loop like the pidfds and pipes; there is no watcher thread. The
//! events themselves are not inspected: any change schedules a rescan, and a
//! rescan of a small folder is cheaper than reasoning about renames.

use std::{
    fs::File,
    io::{ErrorKind, Read},
    path::Path,
};

use anyhow::{Context, anyhow};
use rustix::fs::inotify::{self, CreateFlags, WatchFlags};
use smithay::reexports::calloop::{Interest, LoopHandle, Mode, PostAction, generic::Generic};

use crate::state::Tessera;

/// Starts watching `dir`; changes trigger [`Tessera::schedule_rescan`].
pub fn watch(handle: &LoopHandle<'static, Tessera>, dir: &Path) -> anyhow::Result<()> {
    let fd = inotify::init(CreateFlags::NONBLOCK | CreateFlags::CLOEXEC)
        .context("cannot create an inotify instance")?;
    // CLOSE_WRITE: saved in place. MOVED_TO: saved by rename, as most editors
    // do. ATTRIB: `chmod +x`, which is what makes a file a script.
    let flags = WatchFlags::CREATE
        | WatchFlags::DELETE
        | WatchFlags::MOVED_FROM
        | WatchFlags::MOVED_TO
        | WatchFlags::CLOSE_WRITE
        | WatchFlags::ATTRIB;
    inotify::add_watch(&fd, dir, flags)
        .with_context(|| format!("cannot watch {}", dir.display()))?;

    handle
        .insert_source(
            Generic::new(File::from(fd), Interest::READ, Mode::Level),
            |_, file, state| {
                drain(file);
                state.schedule_rescan();
                Ok(PostAction::Continue)
            },
        )
        .map_err(|err| anyhow!("cannot watch the inotify descriptor: {}", err.error))?;
    Ok(())
}

/// Reads and discards pending events, so the descriptor stops being readable.
fn drain(file: &File) {
    let mut reader: &File = file;
    let mut buffer = [0u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return,
            Ok(_) => continue,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}
