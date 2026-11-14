//! Read-only access to regular files in a single-FAT exFAT root directory.
use crate::{Error, ReadAt};

#[derive(Debug, Eq, PartialEq)]
pub struct Geometry {
    pub sector_size: u64,
    pub sectors_per_cluster: u64,
    pub fat_sector: u64,
    pub heap_sector: u64,
    pub cluster_count: u32,
    pub root_cluster: u32,
}

pub fn read_geometry(
    source: &mut impl ReadAt,
    offset: u64,
    partition_length: u64,
) -> Result<Geometry, Error> {
    if partition_length < 512
        || offset.checked_add(partition_length).ok_or(Error::Bounds)? > source.len()
    {
        return Err(Error::Bounds);
    }
    let mut buffer = [0; 4096];
    source.read_at(offset, &mut buffer[..512])?;
    if &buffer[3..11] != b"EXFAT   "
        || buffer[11..64].iter().any(|&b| b != 0)
        || buffer[510..512] != [0x55, 0xaa]
    {
        return Err(Error::Invalid);
    }
    let sector_shift = buffer[108];
    let cluster_shift = buffer[109];
    if !(9..=12).contains(&sector_shift)
        || cluster_shift > 25 - sector_shift
        || buffer[110] != 1
        || buffer[104..106] != [0, 1]
    {
        return Err(Error::Unsupported);
    }
    let u32_at = |i| u32::from_le_bytes(buffer[i..i + 4].try_into().unwrap());
    let volume_sectors = u64::from_le_bytes(buffer[72..80].try_into().unwrap());
    let geometry = Geometry {
        sector_size: 1 << sector_shift,
        sectors_per_cluster: 1 << cluster_shift,
        fat_sector: u64::from(u32_at(80)),
        heap_sector: u64::from(u32_at(88)),
        cluster_count: u32_at(92),
        root_cluster: u32_at(96),
    };
    let fat_length = u64::from(u32_at(84));
    let cluster_limit = u64::from(geometry.cluster_count) + 2;
    if volume_sectors > partition_length / geometry.sector_size
        || geometry.fat_sector < 24
        || geometry.fat_sector + fat_length > geometry.heap_sector
        || fat_length * geometry.sector_size < cluster_limit * 4
        || geometry.heap_sector + u64::from(geometry.cluster_count) * geometry.sectors_per_cluster
            > volume_sectors
        || geometry.root_cluster < 2
        || u64::from(geometry.root_cluster) >= cluster_limit
    {
        return Err(Error::Bounds);
    }
    let mut checksum = 0u32;
    let sector = &mut buffer[..geometry.sector_size as usize];
    for index in 0..11 {
        source.read_at(offset + index * geometry.sector_size, sector)?;
        for (position, &byte) in sector.iter().enumerate() {
            if index != 0 || !matches!(position, 106 | 107 | 112) {
                checksum = checksum.rotate_right(1).wrapping_add(u32::from(byte));
            }
        }
    }
    source.read_at(offset + 11 * geometry.sector_size, sector)?;
    if !sector
        .as_chunks::<4>()
        .0
        .iter()
        .all(|word| *word == checksum.to_le_bytes())
    {
        return Err(Error::Invalid);
    }
    Ok(geometry)
}

pub struct Volume<R> {
    source: R,
    offset: u64,
    geometry: Geometry,
}

pub struct Entry {
    name: [u16; 255],
    name_len: usize,
    first: u32,
    length: u64,
    initialized: u64,
    contiguous: bool,
}

impl Entry {
    pub fn name(&self) -> &[u16] {
        &self.name[..self.name_len]
    }
}

impl<R: ReadAt> Volume<R> {
    pub fn open(mut source: R, offset: u64, length: u64) -> Result<Self, Error> {
        let geometry = read_geometry(&mut source, offset, length)?;
        Ok(Self {
            source,
            offset,
            geometry,
        })
    }

    fn cluster_size(&self) -> u64 {
        self.geometry.sector_size * self.geometry.sectors_per_cluster
    }

    fn cluster_offset(&self, cluster: u32) -> Result<u64, Error> {
        if cluster < 2 || cluster - 2 >= self.geometry.cluster_count || cluster >= 0xffff_fff7 {
            return Err(Error::Invalid);
        }
        Ok(self.offset
            + self.geometry.heap_sector * self.geometry.sector_size
            + u64::from(cluster - 2) * self.cluster_size())
    }

    fn next(&mut self, cluster: u32) -> Result<Option<u32>, Error> {
        self.cluster_offset(cluster)?;
        let mut raw = [0; 4];
        let offset = self.offset
            + self.geometry.fat_sector * self.geometry.sector_size
            + u64::from(cluster) * 4;
        self.source.read_at(offset, &mut raw)?;
        let next = u32::from_le_bytes(raw);
        if next == u32::MAX {
            return Ok(None);
        }
        self.cluster_offset(next)?;
        Ok(Some(next))
    }

    /// Enumerate up to 16,384 root directory records, including secondary records.
    /// Returning false stops enumeration.
    pub fn visit_root(&mut self, mut visit: impl FnMut(Entry) -> bool) -> Result<(), Error> {
        let mut cluster = self.geometry.root_cluster;
        let mut position = 0;
        let mut record = [0; 32];
        let mut remaining = 16_384usize;
        let mut read_record = |volume: &mut Self, record: &mut [u8; 32]| -> Result<bool, Error> {
            if position == volume.cluster_size() {
                let Some(next) = volume.next(cluster)? else {
                    return Ok(false);
                };
                cluster = next;
                position = 0;
            }
            remaining = remaining.checked_sub(1usize).ok_or(Error::Unsupported)?;
            volume
                .source
                .read_at(volume.cluster_offset(cluster)? + position, record)?;
            position += 32;
            Ok(true)
        };
        while read_record(self, &mut record)? && record[0] != 0 {
            if record[0] != 0x85 {
                continue;
            }
            let count = usize::from(record[1]);
            if !(2..=18).contains(&count) {
                return Err(Error::Unsupported);
            }
            let mut set = [[0; 32]; 19];
            set[0] = record;
            for secondary in &mut set[1..=count] {
                if !read_record(self, secondary)? {
                    return Err(Error::Invalid);
                }
            }
            let entry = parse_entry(&set[..=count])?;
            if set[0][4] & 0x10 == 0 && !visit(entry) {
                return Ok(());
            }
        }
        Ok(())
    }

    pub fn open_file(&mut self, entry: Entry) -> Result<File<'_, R>, Error> {
        let count = entry.length.div_ceil(self.cluster_size());
        if count > u64::from(self.geometry.cluster_count) || entry.initialized > entry.length {
            return Err(Error::Invalid);
        }
        if count == 0 {
            if entry.first != 0 {
                return Err(Error::Invalid);
            }
        } else if entry.contiguous {
            self.cluster_offset(entry.first)?;
            self.cluster_offset(
                entry
                    .first
                    .checked_add((count - 1) as u32)
                    .ok_or(Error::Bounds)?,
            )?;
        } else {
            // Exact termination rejects cycles and chains that run past the file allocation.
            let mut current = Some(entry.first);
            for _ in 0..count {
                current = self.next(current.ok_or(Error::Invalid)?)?;
            }
            if current.is_some() {
                return Err(Error::Invalid);
            }
        }
        let cursor_cluster = entry.first;
        Ok(File {
            volume: self,
            entry,
            cursor_cluster,
            cursor_index: 0,
        })
    }
}

fn parse_entry(set: &[[u8; 32]]) -> Result<Entry, Error> {
    let checksum = set
        .iter()
        .flatten()
        .enumerate()
        .filter(|(index, _)| !matches!(index, 2 | 3))
        .fold(0u16, |sum, (_, &byte)| {
            sum.rotate_right(1).wrapping_add(u16::from(byte))
        });
    let stream = &set[1];
    let name_len = usize::from(stream[3]);
    if checksum != u16::from_le_bytes([set[0][2], set[0][3]])
        || stream[0] != 0xc0
        || stream[1] & !3 != 0
        || stream[1] & 1 == 0
        || name_len == 0
        || set.len() != 2 + name_len.div_ceil(15)
    {
        return Err(Error::Invalid);
    }
    let mut name = [0; 255];
    for (index, record) in set[2..].iter().enumerate() {
        if record[0] != 0xc1 {
            return Err(Error::Invalid);
        }
        for (slot, word) in name[index * 15..name_len]
            .iter_mut()
            .zip(record[2..].as_chunks::<2>().0.iter())
        {
            *slot = u16::from_le_bytes([word[0], word[1]]);
            if *slot == 0 {
                return Err(Error::Invalid);
            }
        }
    }
    Ok(Entry {
        name,
        name_len,
        first: u32::from_le_bytes(stream[20..24].try_into().unwrap()),
        length: u64::from_le_bytes(stream[24..32].try_into().unwrap()),
        initialized: u64::from_le_bytes(stream[8..16].try_into().unwrap()),
        contiguous: stream[1] & 2 != 0,
    })
}

pub struct File<'a, R> {
    volume: &'a mut Volume<R>,
    entry: Entry,
    cursor_cluster: u32,
    cursor_index: u64,
}

impl<R: ReadAt> ReadAt for File<'_, R> {
    fn len(&self) -> u64 {
        self.entry.length
    }

    fn read_at(&mut self, mut offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
        let end = offset
            .checked_add(buffer.len() as u64)
            .ok_or(Error::Bounds)?;
        if end > self.len() {
            return Err(Error::Bounds);
        }
        let initialized = (self.entry.initialized.saturating_sub(offset)).min(buffer.len() as u64);
        let (mut data, zeroes) = buffer.split_at_mut(initialized as usize);
        zeroes.fill(0);
        let size = self.volume.cluster_size();
        while !data.is_empty() {
            let index = offset / size;
            let position = offset % size;
            let cluster = if self.entry.contiguous {
                self.entry
                    .first
                    .checked_add(index as u32)
                    .ok_or(Error::Bounds)?
            } else {
                if index < self.cursor_index {
                    self.cursor_cluster = self.entry.first;
                    self.cursor_index = 0;
                }
                while self.cursor_index < index {
                    self.cursor_cluster = self
                        .volume
                        .next(self.cursor_cluster)?
                        .ok_or(Error::Invalid)?;
                    self.cursor_index += 1;
                }
                self.cursor_cluster
            };
            let address = self.volume.cluster_offset(cluster)? + position;
            let count = data.len().min((size - position) as usize);
            self.volume.source.read_at(address, &mut data[..count])?;
            offset += count as u64;
            data = &mut data[count..];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> [u8; 40 * 512] {
        let mut volume = [0; 40 * 512];
        volume[3..11].copy_from_slice(b"EXFAT   ");
        volume[72..80].copy_from_slice(&40u64.to_le_bytes());
        for (offset, value) in [(80, 24u32), (84, 1), (88, 25), (92, 15), (96, 2)] {
            volume[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        volume[105] = 1;
        volume[108] = 9;
        volume[110] = 1;
        volume[510..512].copy_from_slice(&[0x55, 0xaa]);
        let mut checksum = 0u32;
        for (index, &byte) in volume[..11 * 512].iter().enumerate() {
            if !matches!(index, 106 | 107 | 112) {
                checksum = checksum.rotate_right(1).wrapping_add(u32::from(byte));
            }
        }
        for word in volume[11 * 512..12 * 512].as_chunks_mut::<4>().0.iter_mut() {
            word.copy_from_slice(&checksum.to_le_bytes());
        }
        volume
    }

    #[test]
    fn reads_fragmented_iso_across_clusters_and_rejects_cycles() {
        let mut image = fixture();
        let fat = &mut image[24 * 512..25 * 512];
        fat[8..12].fill(0xff);
        fat[12..16].copy_from_slice(&6u32.to_le_bytes());
        fat[24..28].fill(0xff);
        let set = &mut image[25 * 512..][..96];
        set[0] = 0x85;
        set[1] = 2;
        set[32] = 0xc0;
        set[33] = 1;
        set[35] = 8;
        set[40..48].copy_from_slice(&514u64.to_le_bytes());
        set[52..56].copy_from_slice(&3u32.to_le_bytes());
        set[56..64].copy_from_slice(&520u64.to_le_bytes());
        set[64] = 0xc1;
        for (word, ch) in set[66..]
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip("test.iso".encode_utf16())
        {
            word.copy_from_slice(&ch.to_le_bytes());
        }
        let sum = set
            .iter()
            .enumerate()
            .filter(|(i, _)| !matches!(i, 2 | 3))
            .fold(0u16, |sum, (_, &byte)| {
                sum.rotate_right(1).wrapping_add(u16::from(byte))
            });
        set[2..4].copy_from_slice(&sum.to_le_bytes());
        image[27 * 512 - 2..27 * 512].copy_from_slice(b"AB");
        image[29 * 512..][..4].copy_from_slice(b"CDXX");
        fn open(image: &[u8]) -> (Volume<&[u8]>, Entry) {
            let mut volume = Volume::open(image, 0, image.len() as u64).unwrap();
            let mut entry = None;
            volume
                .visit_root(|value| {
                    entry = Some(value);
                    true
                })
                .unwrap();
            (volume, entry.unwrap())
        }
        let (mut volume, entry) = open(&image);
        assert!(entry.name().iter().copied().eq("test.iso".encode_utf16()));
        let mut file = volume.open_file(entry).unwrap();
        let mut bytes = [0xff; 8];
        file.read_at(510, &mut bytes).unwrap();
        assert_eq!(&bytes, b"ABCD\0\0\0\0");
        file.read_at(510, &mut bytes[..2]).unwrap();
        assert_eq!(&bytes[..2], b"AB");
        image[24 * 512 + 24..][..4].copy_from_slice(&3u32.to_le_bytes());
        let (mut volume, entry) = open(&image);
        assert!(matches!(volume.open_file(entry), Err(Error::Invalid)));
    }

    #[test]
    fn rejects_corrupted_boot_region_and_partition_overrun() {
        let mut volume = fixture();
        let len = volume.len() as u64;
        let geometry = read_geometry(&mut volume.as_slice(), 0, len).unwrap();
        assert_eq!(geometry.root_cluster, 2);
        assert_eq!(geometry.heap_sector, 25);
        assert_eq!(
            read_geometry(&mut volume.as_slice(), 0, len - 512),
            Err(Error::Bounds)
        );
        volume[512] ^= 1;
        assert_eq!(
            read_geometry(&mut volume.as_slice(), 0, len),
            Err(Error::Invalid)
        );
    }
}
