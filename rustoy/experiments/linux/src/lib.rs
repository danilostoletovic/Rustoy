//! Discovery of boot assets in the Debian Live ISO layout.
#![no_std]
#![forbid(unsafe_code)]

use rustoy_fs::{Error, ReadAt};
use rustoy_iso::{Extent, Iso};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DebianLiveAssets {
    pub kernel: Extent,
    pub initrd: Extent,
}

impl DebianLiveAssets {
    /// Locate the files and check the Linux boot protocol signature.
    /// EFI architecture and image admission are the firmware loader's responsibility.
    pub fn locate<R: ReadAt>(iso: &mut Iso<R>) -> Result<Self, Error> {
        let kernel = iso.file("/live/vmlinuz")?;
        let initrd = iso.file("/live/initrd.img")?;
        let mut signatures = [0; 6];
        iso.read(kernel, 0x1fe, &mut signatures)?;
        if signatures != [0x55, 0xaa, b'H', b'd', b'r', b'S'] || initrd.length == 0 {
            return Err(Error::Invalid);
        }
        Ok(Self { kernel, initrd })
    }
}
