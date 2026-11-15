use std::{io, ptr::NonNull};

use libsqlite3_sys::{
    SQLITE_OK, sqlite3_blob, sqlite3_blob_bytes, sqlite3_blob_close, sqlite3_blob_open,
    sqlite3_blob_read,
};
use sqlx::sqlite::LockedSqliteHandle;

use super::{SQLITE_NON_CONTENT_CODES, invalid};
use crate::CorpusError;

pub(super) struct SentenceBlob<'handle, 'connection> {
    handle: NonNull<sqlite3_blob>,
    _connection: &'handle mut LockedSqliteHandle<'connection>,
    len: usize,
    offset: usize,
}

impl<'handle, 'connection> SentenceBlob<'handle, 'connection> {
    pub(super) fn open(
        connection: &'handle mut LockedSqliteHandle<'connection>,
        row: i64,
    ) -> Result<Self, CorpusError> {
        let mut handle = std::ptr::null_mut();
        // SAFETY: SQLx locks the connection for this borrow; names are static C strings,
        // and the returned read-only handle stays within that borrow (sqlite.org/c3ref/blob_open.html).
        let status = unsafe {
            sqlite3_blob_open(
                connection.as_raw_handle().as_ptr(),
                c"main".as_ptr(),
                c"sentence_text".as_ptr(),
                c"block".as_ptr(),
                row,
                0,
                &raw mut handle,
            )
        };
        if status != SQLITE_OK {
            return Err(failure(status));
        }
        let handle =
            NonNull::new(handle).ok_or_else(|| invalid("SQLite returned no blob handle"))?;
        let mut blob = Self {
            handle,
            _connection: connection,
            len: 0,
            offset: 0,
        };
        // SAFETY: the handle was opened successfully and has not been closed.
        let len = unsafe { sqlite3_blob_bytes(blob.handle.as_ptr()) };
        blob.len =
            usize::try_from(len).map_err(|_| invalid("SQLite returned a negative blob length"))?;
        Ok(blob)
    }

    pub(super) const fn len(&self) -> usize {
        self.len
    }
}

impl io::Read for SentenceBlob<'_, '_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = output.len().min(self.len - self.offset).min(64 * 1024);
        if count == 0 {
            return Ok(0);
        }
        // SAFETY: the live handle borrows the locked connection, output holds count
        // writable bytes, and offset + count stays within the blob's i32-sized length.
        let status = unsafe {
            sqlite3_blob_read(
                self.handle.as_ptr(),
                output.as_mut_ptr().cast(),
                count as i32,
                self.offset as i32,
            )
        };
        if status != SQLITE_OK {
            return Err(io::Error::other(failure(status)));
        }
        self.offset += count;
        Ok(count)
    }
}

impl Drop for SentenceBlob<'_, '_> {
    fn drop(&mut self) {
        // SAFETY: this value owns the live handle and closes it before releasing
        // the connection borrow (sqlite.org/c3ref/blob_close.html).
        unsafe {
            sqlite3_blob_close(self.handle.as_ptr());
        }
    }
}

fn failure(code: i32) -> CorpusError {
    let message = format!("SQLite blob access failed with code {code}");
    if SQLITE_NON_CONTENT_CODES.contains(&((code & 0xff) as u32)) {
        CorpusError::Io(io::Error::other(message))
    } else {
        invalid(message)
    }
}
