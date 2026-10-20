use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use lingrep_core::spill::{TempBudget, TempReservation};

use crate::CorpusError;

/// One exact private scratch file charged to a shared temporary-storage budget.
pub(crate) struct BudgetedScratch {
    path: PathBuf,
    file: Option<File>,
    reservation: TempReservation,
}

impl BudgetedScratch {
    pub(crate) fn create(path: &Path, budget: &Arc<TempBudget>) -> Result<Self, CorpusError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            file: Some(file),
            reservation: budget.try_reserve(0)?,
        })
    }

    pub(crate) fn append(&mut self, bytes: &[u8]) -> Result<(), CorpusError> {
        self.reservation.try_grow(bytes.len() as u64)?;
        self.file().write_all(bytes)?;
        Ok(())
    }

    pub(crate) fn write_existing(&mut self, bytes: &[u8]) -> Result<(), CorpusError> {
        self.file().write_all(bytes)?;
        Ok(())
    }

    pub(crate) fn read_exact(&mut self, bytes: &mut [u8]) -> Result<(), CorpusError> {
        self.file().read_exact(bytes)?;
        Ok(())
    }

    pub(crate) fn seek(&mut self, position: SeekFrom) -> Result<u64, CorpusError> {
        Ok(self.file().seek(position)?)
    }

    pub(crate) fn rewind(&mut self) -> Result<(), CorpusError> {
        self.seek(SeekFrom::Start(0))?;
        Ok(())
    }

    pub(crate) fn sync_all(&mut self) -> Result<(), CorpusError> {
        self.file().sync_all()?;
        Ok(())
    }

    fn file(&mut self) -> &mut File {
        self.file.as_mut().expect("scratch file remains open")
    }
}

impl Drop for BudgetedScratch {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}
