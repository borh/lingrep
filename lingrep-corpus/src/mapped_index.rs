use std::{fs::File, io, ops::Deref, path::Path};

use memmap2::Mmap;

pub(crate) struct U64Index {
    map: Option<Mmap>,
}

impl U64Index {
    pub(crate) fn open(path: &Path, count: u64) -> io::Result<Self> {
        let bytes = count
            .checked_mul(8)
            .filter(|bytes| *bytes <= isize::MAX as u64)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} dimensions exceed the addressable size", path.display()),
                )
            })?;
        let file = File::open(path)?;
        if file.metadata()?.len() != bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not {count} u64 values", path.display()),
            ));
        }
        // SAFETY: published component bytes remain immutable for every reader (crate contract).
        let map = (bytes != 0)
            .then(|| unsafe { Mmap::map(&file) })
            .transpose()?;
        if let Some(map) = &map {
            bytemuck::try_cast_slice::<u8, u64>(map)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        }
        Ok(Self { map })
    }
}

impl Deref for U64Index {
    type Target = [u64];

    fn deref(&self) -> &Self::Target {
        self.map
            .as_ref()
            .map_or(&[], |map| bytemuck::cast_slice(map))
    }
}
