#![no_std]
#![forbid(unsafe_code)]

pub mod exfat;
pub mod gpt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Io,
    Bounds,
    Invalid,
    Unsupported,
    NotFound,
}

pub trait ReadAt {
    fn len(&self) -> u64;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fill the entire buffer or return an error.
    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), Error>;
}

impl<R: ReadAt + ?Sized> ReadAt for &mut R {
    fn len(&self) -> u64 {
        (**self).len()
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
        (**self).read_at(offset, buffer)
    }
}

impl ReadAt for &[u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
        let start = usize::try_from(offset).map_err(|_| Error::Bounds)?;
        let end = start.checked_add(buffer.len()).ok_or(Error::Bounds)?;
        buffer.copy_from_slice(self.get(start..end).ok_or(Error::Bounds)?);
        Ok(())
    }
}
