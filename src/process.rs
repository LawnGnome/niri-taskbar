use futures::AsyncReadExt;
use thiserror::Error;
use waybar_cffi::gtk::{
    gio::{File, prelude::InputStreamExtManual, traits::FileExt},
    glib::{self, Priority},
};

/// A running process.
pub struct Process {
    pub ppid: Option<i64>,
}

impl Process {
    /// Instantiates a new process.
    ///
    /// Under the hood, this parses `/proc/{pid}/stat` to get the parent PID,
    /// which is all we care about right now.
    #[tracing::instrument(level = "TRACE", err)]
    pub async fn new(pid: i64) -> Result<Self, Error> {
        // Implementation note: there are any number of crates that can do this,
        // but honestly, most of them are either buggy, introduce a new build
        // dependency, or way heavier than we need.
        //
        // Implementing this ourselves also has the benefit that we can use GIO,
        // which means that we integrate nicely with GLib's event loop for free.
        let stat = File::for_path(format!("/proc/{pid}/stat"));

        // The GIO InputStream interface is fairly byzantine, so we'll use the
        // provided extension trait to turn it into an `AsyncBufRead`, which is
        // much nicer to deal with.
        let mut stream = stat
            .read_future(Priority::DEFAULT)
            .await
            .map_err(|e| Error::Open { e, pid })?
            .into_async_buf_read(4096);

        // It's probably technically possible for the `comm` field to be invalid
        // UTF-8 and break this, but I don't think I care very much, honestly.
        let mut buffer = String::new();
        stream
            .read_to_string(&mut buffer)
            .await
            .map_err(|e| Error::Read { e, pid })?;

        // Per proc_pid_stat(5), the parent PID is the fourth element.
        let ppid = buffer
            .split(' ')
            .nth(3)
            .ok_or_else(|| Error::InsufficientFields { pid })?;

        let ppid = ppid.parse().map_err(|_| Error::ParentMalformedNumber {
            parent: ppid.to_owned(),
            pid: pid,
        })?;

        Ok(Self {
            // Convenience: PPID 0 indicates that the process is an orphan or
            // PID 1, so we'll just convert that into an Option here to make
            // things easier for the caller and encapsulate the arcane /proc
            // knowledge in one place.
            ppid: if ppid == 0 { None } else { Some(ppid) },
        })
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("malformed /proc/{pid}/stat: insufficient fields")]
    InsufficientFields { pid: i64 },

    #[error("parent PID not a valid number in /proc/{pid}/stat: {parent}")]
    ParentMalformedNumber { parent: String, pid: i64 },

    #[error("cannot open /proc/{pid}/stat for read: {e}")]
    Open {
        #[source]
        e: glib::Error,
        pid: i64,
    },

    #[error("error reading from /proc/{pid}/stat: {e}")]
    Read {
        #[source]
        e: futures::io::Error,
        pid: i64,
    },
}
