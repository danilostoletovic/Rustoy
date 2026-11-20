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
        let mut signatures = [0; 8];
        iso.read(kernel, 0x1fe, &mut signatures)?;
        if signatures[..2] != [0x55, 0xaa] || signatures[4..] != *b"HdrS" || initrd.length == 0 {
            return Err(Error::Invalid);
        }
        Ok(Self { kernel, initrd })
    }
}
