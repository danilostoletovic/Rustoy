//! Virtual ISO9660 replacement for a single-extent Windows boot WIM.
use rustoy_fs::{Error, ReadAt};
use rustoy_iso::{Iso, SECTOR_SIZE};

use crate::wim::{BootWim, Overlay, Patch};

pub struct Replacement {
    volume_field: u64,
    volume_size: [u8; 8],
    record_field: u64,
    record: [u8; 16],
    relocated: u64,
    wim_length: u64,
    total: u64,
    original_length: u64,
}

fn dual(value: u32) -> [u8; 8] {
    let mut bytes = [0; 8];
    bytes[..4].copy_from_slice(&value.to_le_bytes());
    bytes[4..].copy_from_slice(&value.to_be_bytes());
    bytes
}

impl Replacement {
    pub fn plan(source: &mut impl ReadAt, wim: &mut impl ReadAt) -> Result<Self, Error> {
        BootWim::read(wim)?;
        let sector = SECTOR_SIZE as u64;
        let original_length = source.len();
        if !original_length.is_multiple_of(sector) {
            return Err(Error::Unsupported);
        }
        // Hybrid media need a separate UDF directory patch. Reject recognized
        // hybrid descriptors rather than advertise contradictory file locations.
        let mut descriptor = [0; 7];
        for block in 16..128 {
            let offset = block * sector;
            if offset + descriptor.len() as u64 > original_length {
                break;
            }
            source.read_at(offset, &mut descriptor)?;
            if matches!(&descriptor[1..6], b"BEA01" | b"NSR02" | b"NSR03") {
                return Err(Error::Unsupported);
            }
        }
        let mut iso = Iso::open(source)?;
        let volume_field = iso.primary_volume_size_field()?;
        let record_field = iso.file_record("/sources/boot.wim")?.offset + 2;
        if record_field < volume_field + 8 {
            return Err(Error::Unsupported);
        }
        let relocated = original_length;
        let wim_length = wim.len();
        let total_sectors = relocated / sector + wim_length.div_ceil(sector);
        let total_sectors = u32::try_from(total_sectors).map_err(|_| Error::Bounds)?;
        let mut record = [0; 16];
        record[..8].copy_from_slice(&dual(
            u32::try_from(relocated / sector).map_err(|_| Error::Bounds)?,
        ));
        record[8..].copy_from_slice(&dual(u32::try_from(wim_length).map_err(|_| Error::Bounds)?));
        Ok(Self {
            volume_field,
            volume_size: dual(total_sectors),
            record_field,
            record,
            relocated,
            wim_length,
            total: u64::from(total_sectors) * sector,
            original_length,
        })
    }

    pub fn view<R: ReadAt, W: ReadAt>(
        &self,
        source: R,
        wim: W,
    ) -> Result<ReplacedIso<'_, R, W>, Error> {
        if source.len() != self.original_length || wim.len() != self.wim_length {
            return Err(Error::Invalid);
        }
        Ok(ReplacedIso {
            plan: self,
            source,
            wim,
        })
    }
}

pub struct ReplacedIso<'a, R, W> {
    plan: &'a Replacement,
    source: R,
    wim: W,
}

impl<R: ReadAt, W: ReadAt> ReadAt for ReplacedIso<'_, R, W> {
    fn len(&self) -> u64 {
        self.plan.total
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
        let plan = self.plan;
        let patches = [
            Patch {
                offset: plan.volume_field,
                bytes: &plan.volume_size,
            },
            Patch {
                offset: plan.record_field,
                bytes: &plan.record,
            },
        ];
        let mut view = Overlay::new(&mut self.source, plan.total, &patches)?;
        view.read_at(offset, buffer)?;
        let start = offset.max(plan.relocated);
        let end = (offset + buffer.len() as u64).min(plan.relocated + plan.wim_length);
        if start < end {
            self.wim.read_at(
                start - plan.relocated,
                &mut buffer[(start - offset) as usize..(end - offset) as usize],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(out: &mut [u8], name: &[u8], sector: u32, size: u32, directory: bool) {
        out[0] = (33 + name.len() + usize::from(name.len().is_multiple_of(2))) as u8;
        out[2..10].copy_from_slice(&dual(sector));
        out[10..18].copy_from_slice(&dual(size));
        out[25] = if directory { 2 } else { 0 };
        out[28..32].copy_from_slice(&[1, 0, 0, 1]);
        out[32] = name.len() as u8;
        out[33..33 + name.len()].copy_from_slice(name);
    }

    #[test]
    fn virtual_directory_resolves_replacement_without_changing_original() {
        let mut disk = [0; 24 * SECTOR_SIZE];
        let pvd = &mut disk[16 * SECTOR_SIZE..][..SECTOR_SIZE];
        pvd[..7].copy_from_slice(b"\x01CD001\x01");
        pvd[80..88].copy_from_slice(&dual(24));
        pvd[128..132].copy_from_slice(&[0, 8, 8, 0]);
        record(&mut pvd[156..], &[0], 18, 2048, true);
        disk[17 * SECTOR_SIZE..][..7].copy_from_slice(b"\xffCD001\x01");
        record(&mut disk[18 * SECTOR_SIZE..], b"SOURCES", 19, 2048, true);
        record(&mut disk[19 * SECTOR_SIZE..], b"BOOT.WIM;1", 20, 16, false);
        disk[20 * SECTOR_SIZE] = 42;
        let mut wim = [0; 289];
        wim[..8].copy_from_slice(b"MSWIM\0\0\0");
        wim[8..12].copy_from_slice(&208u32.to_le_bytes());
        wim[40..44].copy_from_slice(&[1, 0, 1, 0]);
        wim[44] = 1;
        wim[120] = 1;
        for (entry, start, flags) in [(48, 208u64, 0), (96, 240, 2)] {
            wim[entry] = 16;
            wim[entry + 7] = flags;
            wim[entry + 8..entry + 16].copy_from_slice(&start.to_le_bytes());
            wim[entry + 16] = 16;
        }
        wim[288] = 99;
        let plan = Replacement::plan(&mut disk.as_slice(), &mut wim.as_slice()).unwrap();
        let view = plan.view(disk.as_slice(), wim.as_slice()).unwrap();
        assert_eq!(view.len(), 25 * SECTOR_SIZE as u64);
        let mut patched = Iso::open(view).unwrap();
        let file = patched.file("/sources/boot.wim").unwrap();
        assert_eq!(file.offset, disk.len() as u64);
        assert_eq!(file.length, wim.len() as u64);
        let mut read = [0; 289];
        patched.read(file, 0, &mut read).unwrap();
        assert_eq!(read, wim);
        let mut original = Iso::open(disk.as_slice()).unwrap();
        assert_eq!(original.file("/sources/boot.wim").unwrap().length, 16);
        assert_eq!(disk[20 * SECTOR_SIZE], 42);
        disk[23 * SECTOR_SIZE + 1..][..5].copy_from_slice(b"NSR03");
        assert!(matches!(
            Replacement::plan(&mut disk.as_slice(), &mut wim.as_slice()),
            Err(Error::Unsupported)
        ));
    }
}
