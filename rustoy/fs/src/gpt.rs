//! GPT header and partition table validation for disk inspection.
use crate::{Error, ReadAt};

#[derive(Debug, Eq, PartialEq)]
pub struct Header {
    pub disk_id: [u8; 16],
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    pub entries_lba: u64,
    pub entry_count: u32,
    pub entry_size: u32,
    pub entries_crc: u32,
}

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

pub fn read_primary(source: &mut impl ReadAt, block_size: usize) -> Result<Header, Error> {
    read_header(source, block_size, false)
}

fn read_header(source: &mut impl ReadAt, block_size: usize, backup: bool) -> Result<Header, Error> {
    if !matches!(block_size, 512 | 4096) {
        return Err(Error::Unsupported);
    }
    if !source.len().is_multiple_of(block_size as u64) || source.len() < 6 * block_size as u64 {
        return Err(Error::Bounds);
    }
    let disk_blocks = source.len() / block_size as u64;
    let mut buffer = [0; 4096];
    let block = &mut buffer[..block_size];
    let (current, alternate) = if backup {
        (disk_blocks - 1, 1)
    } else {
        (1, disk_blocks - 1)
    };
    source.read_at(current * block_size as u64, block)?;
    if &block[..8] != b"EFI PART" || block[8..12] != [0, 0, 1, 0] {
        return Err(Error::Invalid);
    }
    let u32_at =
        |block: &[u8], offset| u32::from_le_bytes(block[offset..offset + 4].try_into().unwrap());
    let u64_at =
        |block: &[u8], offset| u64::from_le_bytes(block[offset..offset + 8].try_into().unwrap());
    let header_size = u32_at(block, 12) as usize;
    if !(92..=block_size).contains(&header_size) {
        return Err(Error::Invalid);
    }
    let expected_crc = u32_at(block, 16);
    block[16..20].fill(0);
    if crc32(&block[..header_size]) != expected_crc
        || u32_at(block, 20) != 0
        || u64_at(block, 24) != current
        || u64_at(block, 32) != alternate
    {
        return Err(Error::Invalid);
    }
    let header = Header {
        first_usable_lba: u64_at(block, 40),
        last_usable_lba: u64_at(block, 48),
        disk_id: block[56..72].try_into().unwrap(),
        entries_lba: u64_at(block, 72),
        entry_count: u32_at(block, 80),
        entry_size: u32_at(block, 84),
        entries_crc: u32_at(block, 88),
    };
    if header.entries_lba < 2
        || header.first_usable_lba > header.last_usable_lba
        || header.last_usable_lba >= disk_blocks - 1
        || header.disk_id == [0; 16]
        || header.entry_count == 0
        || header.entry_size < 128
        || !header.entry_size.is_power_of_two()
    {
        return Err(Error::Invalid);
    }
    let table_blocks =
        (u64::from(header.entry_count) * u64::from(header.entry_size)).div_ceil(block_size as u64);
    let table_end = header
        .entries_lba
        .checked_add(table_blocks)
        .ok_or(Error::Bounds)?;
    if if backup {
        header.entries_lba <= header.last_usable_lba || table_end > current
    } else {
        table_end > header.first_usable_lba
    } {
        return Err(Error::Bounds);
    }
    Ok(header)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Partition {
    pub slot: u32,
    pub kind: [u8; 16],
    pub id: [u8; 16],
    pub first_lba: u64,
    pub last_lba: u64,
}

pub const EFI_SYSTEM: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];
pub const BASIC_DATA: [u8; 16] = [
    0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44, 0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26, 0x99, 0xc7,
];

pub const MAX_TABLE_BYTES: usize = 128 * 1024;

/// Verify both copies and parse the exact bytes covered by the table CRC.
/// Table and output memory are supplied by the caller.
pub fn read_layout<'a>(
    source: &mut impl ReadAt,
    block_size: usize,
    table: &mut [u8],
    partitions: &'a mut [Partition],
) -> Result<(Header, &'a [Partition]), Error> {
    let primary = read_primary(source, block_size)?;
    let mut backup = read_header(source, block_size, true)?;
    let backup_entries_lba = backup.entries_lba;
    backup.entries_lba = primary.entries_lba;
    if primary != backup {
        return Err(Error::Invalid);
    }
    let length = usize::try_from(u64::from(primary.entry_count) * u64::from(primary.entry_size))
        .map_err(|_| Error::Unsupported)?;
    if length > MAX_TABLE_BYTES {
        return Err(Error::Unsupported);
    }
    let table = table.get_mut(..length).ok_or(Error::Bounds)?;
    source.read_at(primary.entries_lba * block_size as u64, table)?;
    if crc32(table) != primary.entries_crc {
        return Err(Error::Invalid);
    }
    let mut scratch = [0; 4096];
    for (index, chunk) in table.chunks(scratch.len()).enumerate() {
        let bytes = &mut scratch[..chunk.len()];
        source.read_at(
            backup_entries_lba * block_size as u64 + (index * 4096) as u64,
            bytes,
        )?;
        if bytes != chunk {
            return Err(Error::Invalid);
        }
    }
    let mut count = 0;
    for (slot, entry) in table.chunks_exact(primary.entry_size as usize).enumerate() {
        let kind = entry[..16].try_into().unwrap();
        if kind == [0; 16] {
            continue;
        }
        let partition = Partition {
            slot: slot as u32 + 1,
            kind,
            id: entry[16..32].try_into().unwrap(),
            first_lba: u64::from_le_bytes(entry[32..40].try_into().unwrap()),
            last_lba: u64::from_le_bytes(entry[40..48].try_into().unwrap()),
        };
        if partition.id == [0; 16]
            || partition.first_lba < primary.first_usable_lba
            || partition.last_lba > primary.last_usable_lba
            || partition.first_lba > partition.last_lba
            || partitions[..count].iter().any(|other| {
                partition.id == other.id
                    || (partition.first_lba <= other.last_lba
                        && other.first_lba <= partition.last_lba)
            })
        {
            return Err(Error::Invalid);
        }
        *partitions.get_mut(count).ok_or(Error::Bounds)? = partition;
        count += 1;
    }
    Ok((primary, &partitions[..count]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout_fixture() -> [u8; 64 * 512] {
        let mut disk = [0; 64 * 512];
        let mut table = [0; 512];
        table[..16].copy_from_slice(&EFI_SYSTEM);
        table[16] = 1;
        table[32] = 10;
        table[40] = 15;
        table[128..144].copy_from_slice(&BASIC_DATA);
        table[144] = 2;
        table[160] = 16;
        table[168] = 60;
        disk[2 * 512..3 * 512].copy_from_slice(&table);
        disk[62 * 512..63 * 512].copy_from_slice(&table);
        for (current, alternate, entries) in [(1u64, 63u64, 2u64), (63, 1, 62)] {
            let block = &mut disk[current as usize * 512..][..512];
            block[..8].copy_from_slice(b"EFI PART");
            block[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
            block[12..16].copy_from_slice(&92u32.to_le_bytes());
            for (offset, value) in [
                (24, current),
                (32, alternate),
                (40, 3),
                (48, 61),
                (72, entries),
            ] {
                block[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            }
            block[56] = 42;
            block[80..84].copy_from_slice(&4u32.to_le_bytes());
            block[84..88].copy_from_slice(&128u32.to_le_bytes());
            block[88..92].copy_from_slice(&crc32(&table).to_le_bytes());
            let crc = crc32(&block[..92]);
            block[16..20].copy_from_slice(&crc.to_le_bytes());
        }
        disk
    }

    #[test]
    fn reads_two_partitions_and_rejects_damage_in_either_table() {
        let disk = layout_fixture();
        let mut table = [0; 512];
        let mut partitions = [Partition {
            slot: 0,
            kind: [0; 16],
            id: [0; 16],
            first_lba: 0,
            last_lba: 0,
        }; 4];
        let (_, found) =
            read_layout(&mut disk.as_slice(), 512, &mut table, &mut partitions).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].kind, EFI_SYSTEM);
        assert_eq!(found[1].kind, BASIC_DATA);
        assert_eq!(found[1].first_lba, 16);
        for sector in [2, 62] {
            let mut corrupted = disk;
            corrupted[sector * 512] ^= 1;
            assert_eq!(
                read_layout(&mut corrupted.as_slice(), 512, &mut table, &mut partitions),
                Err(Error::Invalid)
            );
        }
    }

    #[test]
    fn validates_header_crc_and_partition_table_bounds() {
        let mut disk = [0; 64 * 512];
        let block = &mut disk[512..1024];
        block[..8].copy_from_slice(b"EFI PART");
        block[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        block[12..16].copy_from_slice(&92u32.to_le_bytes());
        for (offset, value) in [(24, 1u64), (32, 63), (40, 34), (48, 61), (72, 2)] {
            block[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        block[56] = 42;
        block[80..84].copy_from_slice(&128u32.to_le_bytes());
        block[84..88].copy_from_slice(&128u32.to_le_bytes());
        let crc = crc32(&block[..92]);
        block[16..20].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(
            read_primary(&mut disk.as_slice(), 512).unwrap().entries_lba,
            2
        );
        disk[512 + 40] = 33;
        assert_eq!(read_primary(&mut disk.as_slice(), 512), Err(Error::Invalid));
        disk[512 + 16..512 + 20].fill(0);
        let crc = crc32(&disk[512..512 + 92]);
        disk[512 + 16..512 + 20].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(read_primary(&mut disk.as_slice(), 512), Err(Error::Bounds));
    }
}
