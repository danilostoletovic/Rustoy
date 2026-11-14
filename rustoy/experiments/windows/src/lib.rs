//! Locate the optical EFI boot filesystem used by Windows installation media.
#![no_std]
#![forbid(unsafe_code)]

use rustoy_fs::{Error, ReadAt};
use rustoy_iso::{Extent, Iso, optical_efi_image};

pub fn probe_iso9660_boot_wim<R: ReadAt>(iso: &mut Iso<R>) -> Result<wim::BootWim, Error> {
    let extent = iso.file("/sources/boot.wim")?;
    struct WimFile<'a, R>(&'a mut Iso<R>, Extent);
    impl<R: ReadAt> ReadAt for WimFile<'_, R> {
        fn len(&self) -> u64 {
            self.1.length
        }

        fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
            self.0.read(self.1, offset, buffer)
        }
    }
    wim::BootWim::read(&mut WimFile(iso, extent))
}

pub struct OpticalMedia<R>(R);

impl<R: ReadAt> OpticalMedia<R> {
    pub const BLOCK_SIZE: usize = 2048;

    pub fn open(source: R) -> Result<Self, Error> {
        if source.is_empty() || !source.len().is_multiple_of(Self::BLOCK_SIZE as u64) {
            return Err(Error::Invalid);
        }
        Ok(Self(source))
    }

    pub fn block_count(&self) -> u64 {
        self.0.len() / Self::BLOCK_SIZE as u64
    }

    pub fn read_blocks(&mut self, lba: u64, buffer: &mut [u8]) -> Result<(), Error> {
        if !buffer.len().is_multiple_of(Self::BLOCK_SIZE) {
            return Err(Error::Invalid);
        }
        let offset = lba
            .checked_mul(Self::BLOCK_SIZE as u64)
            .ok_or(Error::Bounds)?;
        if offset
            .checked_add(buffer.len() as u64)
            .ok_or(Error::Bounds)?
            > self.0.len()
        {
            return Err(Error::Bounds);
        }
        self.0.read_at(offset, buffer)
    }
}

/// Return the full FAT boot filesystem range from its BPB. The El Torito
/// initial-load size may describe only the first part of that filesystem.
pub fn efi_boot_filesystem(source: &mut impl ReadAt) -> Result<Extent, Error> {
    let initial = optical_efi_image(source)?;
    let mut bpb = [0; 512];
    source.read_at(initial.offset, &mut bpb)?;
    if !matches!(bpb[0], 0xeb | 0xe9) || bpb[510..] != [0x55, 0xaa] {
        return Err(Error::Invalid);
    }
    let sector = u16::from_le_bytes([bpb[11], bpb[12]]);
    let cluster = bpb[13];
    let reserved = u16::from_le_bytes([bpb[14], bpb[15]]);
    let short = u32::from(u16::from_le_bytes([bpb[19], bpb[20]]));
    let long = u32::from_le_bytes(bpb[32..36].try_into().unwrap());
    if !matches!(sector, 512 | 1024 | 2048 | 4096)
        || !cluster.is_power_of_two()
        || reserved == 0
        || !matches!(bpb[16], 1 | 2)
        || (short == 0) == (long == 0)
    {
        return Err(Error::Invalid);
    }
    let length = u64::from(short.max(long)) * u64::from(sector);
    if length < initial.length || initial.offset + length > source.len() {
        return Err(Error::Bounds);
    }
    Ok(Extent {
        offset: initial.offset,
        length,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustoy_iso::SECTOR_SIZE;

    #[test]
    fn probes_udf_media_without_an_iso9660_primary_volume() {
        let mut image = [0u8; 24 * SECTOR_SIZE];
        let boot = &mut image[16 * SECTOR_SIZE..][..SECTOR_SIZE];
        boot[1..7].copy_from_slice(b"CD001\x01");
        boot[7..30].copy_from_slice(b"EL TORITO SPECIFICATION");
        boot[71..75].copy_from_slice(&20u32.to_le_bytes());
        for (sector, signature) in [(17, b"BEA01"), (18, b"NSR03"), (19, b"TEA01")] {
            image[sector * SECTOR_SIZE + 1..][..5].copy_from_slice(signature);
        }
        let catalog = &mut image[20 * SECTOR_SIZE..][..SECTOR_SIZE];
        catalog[0] = 1;
        catalog[1] = 0xef;
        catalog[30..32].copy_from_slice(&[0x55, 0xaa]);
        catalog[28..30].copy_from_slice(
            &0u16
                .wrapping_sub(0xef01u16.wrapping_add(0xaa55))
                .to_le_bytes(),
        );
        catalog[32] = 0x88;
        catalog[38] = 1;
        catalog[40] = 21;
        let bpb = &mut image[21 * SECTOR_SIZE..][..512];
        bpb[0] = 0xeb;
        bpb[11..13].copy_from_slice(&512u16.to_le_bytes());
        bpb[13] = 1;
        bpb[14] = 1;
        bpb[16] = 2;
        bpb[19] = 8;
        bpb[510..].copy_from_slice(&[0x55, 0xaa]);
        let source = &mut image.as_slice();
        assert_eq!(
            efi_boot_filesystem(source),
            Ok(Extent {
                offset: (21 * SECTOR_SIZE) as u64,
                length: 4096
            })
        );
        image[21 * SECTOR_SIZE + 19] = 13;
        assert_eq!(
            efi_boot_filesystem(&mut image.as_slice()),
            Err(Error::Bounds)
        );
    }

    #[test]
    fn optical_reads_use_iso_sectors_and_reject_out_of_range_requests() {
        let mut image = [0; 4096];
        image[2048] = 42;
        let mut media = OpticalMedia::open(image.as_slice()).unwrap();
        assert_eq!(media.block_count(), 2);
        let mut buffer = [0; 2048];
        media.read_blocks(1, &mut buffer).unwrap();
        assert_eq!(buffer[0], 42);
        assert_eq!(media.read_blocks(2, &mut buffer), Err(Error::Bounds));
        assert_eq!(
            media.read_blocks(0, &mut buffer[..512]),
            Err(Error::Invalid)
        );
    }
}

pub mod iso_redirect;
pub mod wim;
