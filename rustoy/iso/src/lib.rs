#![no_std]
#![forbid(unsafe_code)]

use rustoy_fs::{Error, ReadAt};

pub const SECTOR_SIZE: usize = 2048;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Extent {
    pub offset: u64,
    pub length: u64,
}

pub struct Iso<R> {
    source: R,
    volume: Extent,
    root: Extent,
    catalog: Option<u32>,
    primary_offset: u64,
    supplementary: bool,
}

pub struct FileRecord {
    pub extent: Extent,
    /// Offset of the directory record, before the extent and length fields.
    pub offset: u64,
}

fn both32(bytes: &[u8]) -> Result<u32, Error> {
    let little = u32::from_le_bytes(bytes[..4].try_into().map_err(|_| Error::Invalid)?);
    let big = u32::from_be_bytes(bytes[4..8].try_into().map_err(|_| Error::Invalid)?);
    if little != big {
        return Err(Error::Invalid);
    }
    Ok(little)
}

fn extent(record: &[u8], limit: u64) -> Result<Extent, Error> {
    if record.len() < 34 || record[1] != 0 || record[26] != 0 || record[27] != 0 {
        return Err(Error::Unsupported);
    }
    let offset = u64::from(both32(&record[2..10])?) * SECTOR_SIZE as u64;
    let length = u64::from(both32(&record[10..18])?);
    if offset + length > limit {
        return Err(Error::Bounds);
    }
    Ok(Extent { offset, length })
}

impl<R: ReadAt> Iso<R> {
    pub fn open(mut source: R) -> Result<Self, Error> {
        let mut block = [0; SECTOR_SIZE];
        let mut primary = None;
        let mut catalog = None;
        let mut supplementary = false;
        for sector in 16..128 {
            source.read_at(sector * SECTOR_SIZE as u64, &mut block)?;
            if &block[1..6] != b"CD001" || block[6] != 1 {
                return Err(Error::Invalid);
            }
            match block[0] {
                0 if block[7..39]
                    .strip_prefix(b"EL TORITO SPECIFICATION")
                    .is_some_and(|tail| tail.iter().all(|&byte| byte == 0 || byte == b' ')) =>
                {
                    catalog = Some(u32::from_le_bytes(block[71..75].try_into().unwrap()));
                }
                1 => {
                    if primary.is_some() || block[128..132] != [0, 8, 8, 0] {
                        return Err(Error::Unsupported);
                    }
                    let length = u64::from(both32(&block[80..88])?) * SECTOR_SIZE as u64;
                    if length > source.len() || length < (sector + 1) * SECTOR_SIZE as u64 {
                        return Err(Error::Bounds);
                    }
                    let root = extent(&block[156..190], length)?;
                    if block[156] != 34 || block[181] & 2 == 0 || root.length == 0 {
                        return Err(Error::Invalid);
                    }
                    primary = Some((
                        Extent { offset: 0, length },
                        root,
                        sector * SECTOR_SIZE as u64,
                    ));
                }
                2 => supplementary = true,
                255 => {
                    let (volume, root, primary_offset) = primary.ok_or(Error::Invalid)?;
                    if (sector + 1) * SECTOR_SIZE as u64 > volume.length {
                        return Err(Error::Bounds);
                    }
                    return Ok(Self {
                        source,
                        volume,
                        root,
                        catalog,
                        primary_offset,
                        supplementary,
                    });
                }
                _ => {}
            }
        }
        Err(Error::Invalid)
    }

    pub fn has_el_torito(&self) -> bool {
        self.catalog.is_some()
    }

    /// Locate a bootable no-emulation EFI entry and validate its initial load range.
    /// The catalog sector count is expressed in 512-byte units.
    pub fn efi_initial_image(&mut self) -> Result<Extent, Error> {
        let catalog_sector = self.catalog.ok_or(Error::NotFound)?;
        read_efi_catalog(&mut self.source, catalog_sector, self.volume.length)
    }
}

/// Locate an EFI boot image without requiring an ISO9660 primary volume.
/// Windows media can use UDF for the data filesystem.
pub fn optical_efi_image(source: &mut impl ReadAt) -> Result<Extent, Error> {
    let mut block = [0; SECTOR_SIZE];
    let mut catalog = None;
    let mut udf = false;
    for sector in 16..128 {
        let offset = sector * SECTOR_SIZE as u64;
        if offset + SECTOR_SIZE as u64 > source.len() {
            break;
        }
        source.read_at(offset, &mut block)?;
        match &block[1..6] {
            b"CD001" if block[6] == 1 => {
                if block[0] == 0
                    && block[7..39]
                        .strip_prefix(b"EL TORITO SPECIFICATION")
                        .is_some_and(|tail| tail.iter().all(|&b| matches!(b, 0 | b' ')))
                {
                    let sector = u32::from_le_bytes(block[71..75].try_into().unwrap());
                    if catalog.replace(sector).is_some() {
                        return Err(Error::Invalid);
                    }
                }
                if block[0] == 255 && catalog.is_some() {
                    break;
                }
            }
            b"BEA01" => udf = true,
            b"NSR02" | b"NSR03" if udf => {}
            b"TEA01" if udf && catalog.is_some() => break,
            _ => {}
        }
    }
    let limit = source.len();
    read_efi_catalog(source, catalog.ok_or(Error::NotFound)?, limit)
}

fn read_efi_catalog(
    source: &mut impl ReadAt,
    catalog_sector: u32,
    limit: u64,
) -> Result<Extent, Error> {
    let mut catalog = [0; SECTOR_SIZE];
    let offset = u64::from(catalog_sector) * SECTOR_SIZE as u64;
    if catalog_sector < 16 || offset + SECTOR_SIZE as u64 > limit {
        return Err(Error::Bounds);
    }
    source.read_at(offset, &mut catalog)?;
    let validation = &catalog[..32];
    let checksum = validation
        .as_chunks::<2>()
        .0
        .iter()
        .fold(0u16, |sum, word| {
            sum.wrapping_add(u16::from_le_bytes([word[0], word[1]]))
        });
    if validation[0] != 1 || validation[30..32] != [0x55, 0xaa] || checksum != 0 {
        return Err(Error::Invalid);
    }
    if validation[1] == 0xef && catalog[32] == 0x88 {
        return boot_extent(&catalog[32..64], limit);
    }
    let mut records = catalog[64..].as_chunks::<32>().0.iter();
    while let Some(header) = records.next() {
        if header[0] == 0 {
            break;
        }
        if !matches!(header[0], 0x90 | 0x91) {
            return Err(Error::Invalid);
        }
        let count = usize::from(u16::from_le_bytes([header[2], header[3]]));
        if count == 0 || count > records.len() {
            return Err(Error::Unsupported);
        }
        for record in records.by_ref().take(count) {
            if header[1] == 0xef && record[0] == 0x88 && record[1] == 0 {
                return boot_extent(record, limit);
            }
        }
        if header[0] == 0x91 {
            break;
        }
    }
    Err(Error::NotFound)
}

fn boot_extent(record: &[u8], limit: u64) -> Result<Extent, Error> {
    if record[1] != 0 {
        return Err(Error::Unsupported);
    }
    let length = u64::from(u16::from_le_bytes([record[6], record[7]])) * 512;
    let sector = u32::from_le_bytes(record[8..12].try_into().unwrap());
    let offset = u64::from(sector) * SECTOR_SIZE as u64;
    if length == 0 || sector == 0 || offset + length > limit {
        return Err(Error::Bounds);
    }
    Ok(Extent { offset, length })
}

impl<R: ReadAt> Iso<R> {
    pub fn read(&mut self, extent: Extent, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
        if offset
            .checked_add(buffer.len() as u64)
            .ok_or(Error::Bounds)?
            > extent.length
            || extent
                .offset
                .checked_add(extent.length)
                .ok_or(Error::Bounds)?
                > self.volume.length
        {
            return Err(Error::Bounds);
        }
        self.source.read_at(extent.offset + offset, buffer)
    }

    pub fn file(&mut self, path: &str) -> Result<Extent, Error> {
        Ok(self.file_record(path)?.extent)
    }

    pub fn primary_volume_size_field(&self) -> Result<u64, Error> {
        if self.supplementary {
            return Err(Error::Unsupported);
        }
        Ok(self.primary_offset + 80)
    }

    pub fn file_record(&mut self, path: &str) -> Result<FileRecord, Error> {
        let mut components = path.strip_prefix('/').unwrap_or(path).split('/').peekable();
        let mut directory = self.root;
        while let Some(name) = components.next() {
            if name.is_empty() || matches!(name, "." | "..") || !name.is_ascii() {
                return Err(Error::Invalid);
            }
            let (found, is_directory) = self.entry(directory, name.as_bytes())?;
            if components.peek().is_none() {
                return if is_directory {
                    Err(Error::NotFound)
                } else {
                    Ok(found)
                };
            }
            if !is_directory {
                return Err(Error::NotFound);
            }
            directory = found.extent;
        }
        Err(Error::NotFound)
    }

    fn entry(&mut self, directory: Extent, name: &[u8]) -> Result<(FileRecord, bool), Error> {
        if directory.length > 32 * 1024 * 1024 {
            return Err(Error::Unsupported);
        }
        let mut block = [0; SECTOR_SIZE];
        for offset in (0..directory.length).step_by(SECTOR_SIZE) {
            let size = (directory.length - offset).min(SECTOR_SIZE as u64) as usize;
            self.read(directory, offset, &mut block[..size])?;
            let mut records = &block[..size];
            while records.first().is_some_and(|&length| length != 0) {
                let length = usize::from(records[0]);
                let record = records
                    .get(..length)
                    .filter(|r| r.len() >= 34)
                    .ok_or(Error::Invalid)?;
                let id = record
                    .get(33..33 + usize::from(record[32]))
                    .ok_or(Error::Invalid)?;
                let id = id.split(|&byte| byte == b';').next().unwrap_or_default();
                if id.eq_ignore_ascii_case(name)
                    || id
                        .strip_suffix(b".")
                        .is_some_and(|base| base.eq_ignore_ascii_case(name))
                {
                    if record[25] & 0x80 != 0 {
                        return Err(Error::Unsupported);
                    }
                    let location = directory.offset + offset + (size - records.len()) as u64;
                    return Ok((
                        FileRecord {
                            extent: extent(record, self.volume.length)?,
                            offset: location,
                        },
                        record[25] & 2 != 0,
                    ));
                }
                records = &records[length..];
            }
        }
        Err(Error::NotFound)
    }
}
