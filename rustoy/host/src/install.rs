//! Assemble an image from two already formatted partition images.
use rustoy_fs::{exfat, gpt};
use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};

const SECTOR: u64 = 512;
const TABLE_SECTORS: u64 = 32;
pub(crate) type ImagePartition<'a> = ([u8; 16], u64, u64, &'a mut dyn Read);

pub fn assemble(output: &OsStr, esp: &OsStr, data: &OsStr, id: &OsStr) -> io::Result<()> {
    let mut esp = super::open_image(esp).map_err(io::Error::other)?;
    let mut data = super::open_image(data).map_err(io::Error::other)?;
    let id = parse_id(id)?;
    let mut boot = [0; 512];
    esp.file.read_exact(&mut boot)?;
    esp.file.rewind()?;
    if boot[82..90] != *b"FAT32   "
        || boot[510..] != [0x55, 0xaa]
        || boot[11..13] != 512u16.to_le_bytes()
        || u64::from(u32::from_le_bytes(boot[32..36].try_into().unwrap())) * SECTOR != esp.length
        || data.length % SECTOR != 0
    {
        return Err(io::Error::other(
            "expected 512-byte-sector FAT32 and exFAT partition images",
        ));
    }
    let length = data.length;
    exfat::read_geometry(&mut data, 0, length)
        .map_err(|err| io::Error::other(format!("exFAT partition validation failed: {err:?}")))?;
    data.file.rewind()?;
    let esp_end = 2048 + esp.length / SECTOR;
    let data_start = esp_end.div_ceil(2048) * 2048;
    write_image(
        output,
        id,
        [0; 512],
        [
            (
                gpt::EFI_SYSTEM,
                2048,
                esp.length,
                &mut esp.file as &mut dyn Read,
            ),
            (gpt::BASIC_DATA, data_start, data.length, &mut data.file),
        ],
    )
}

pub(crate) fn write_image(
    output: &OsStr,
    id: [u8; 16],
    mut mbr: [u8; 512],
    parts: [ImagePartition<'_>; 2],
) -> io::Result<()> {
    let data_end = parts[1].1 + parts[1].2 / SECTOR;
    let last = data_end + TABLE_SECTORS;
    let disk_bytes = (last + 1)
        .checked_mul(SECTOR)
        .ok_or_else(|| io::Error::other("disk image too large"))?;
    let mut table = [0; 16_384];
    for (index, (kind, start, length, _)) in parts.iter().enumerate() {
        let entry = &mut table[index * 128..][..128];
        let mut partition_id = id;
        partition_id[15] ^= index as u8 + 1;
        entry[..16].copy_from_slice(kind);
        entry[16..32].copy_from_slice(&partition_id);
        entry[32..40].copy_from_slice(&start.to_le_bytes());
        entry[40..48].copy_from_slice(&(start + length / SECTOR - 1).to_le_bytes());
    }
    mbr[446..510].fill(0);
    mbr[447..450].copy_from_slice(&[0, 2, 0]);
    mbr[450] = 0xee;
    mbr[451..454].fill(0xff);
    mbr[454..458].copy_from_slice(&1u32.to_le_bytes());
    mbr[458..462].copy_from_slice(&(last.min(u64::from(u32::MAX)) as u32).to_le_bytes());
    mbr[510..].copy_from_slice(&[0x55, 0xaa]);

    let mut disk = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(output)?;
    disk.set_len(disk_bytes)?;
    disk.write_all(&mbr)?;
    for (current, alternate, entries) in [(1, last, 2), (last, 1, data_end)] {
        let mut header = [0; 512];
        header[..8].copy_from_slice(b"EFI PART");
        for (offset, value) in [
            (8, 0x10000),
            (12, 92),
            (80, 128),
            (84, 128),
            (88, gpt::crc32(&table)),
        ] {
            header[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (offset, value) in [
            (24, current),
            (32, alternate),
            (40, 34),
            (48, data_end - 1),
            (72, entries),
        ] {
            header[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        header[56..72].copy_from_slice(&id);
        let checksum = gpt::crc32(&header[..92]);
        header[16..20].copy_from_slice(&checksum.to_le_bytes());
        disk.seek(SeekFrom::Start(current * SECTOR))?;
        disk.write_all(&header)?;
        disk.seek(SeekFrom::Start(entries * SECTOR))?;
        disk.write_all(&table)?;
    }
    for (_, start, length, input) in parts {
        disk.seek(SeekFrom::Start(start * SECTOR))?;
        if io::copy(&mut input.take(length), &mut disk)? != length {
            return Err(io::Error::other(
                "partition source was truncated during copy",
            ));
        }
    }
    disk.sync_all()?;
    let mut result = super::Image {
        file: disk,
        length: disk_bytes,
    };
    let mut table_buffer = [0; 16_384];
    let empty = gpt::Partition {
        slot: 0,
        kind: [0; 16],
        id: [0; 16],
        first_lba: 0,
        last_lba: 0,
    };
    let mut partitions = [empty; 2];
    let (header, found) = gpt::read_layout(
        &mut result,
        SECTOR as usize,
        &mut table_buffer,
        &mut partitions,
    )
    .map_err(|err| io::Error::other(format!("Written GPT verification failed: {err:?}")))?;
    if header.disk_id != id || found.len() != 2 || table_buffer != table {
        return Err(io::Error::other(
            "Written GPT differs from the requested layout",
        ));
    }
    Ok(())
}

pub(crate) fn parse_id(id: &OsStr) -> io::Result<[u8; 16]> {
    let err = || io::Error::other("disk ID must contain 32 hex digits and a nonzero first byte");
    let id = id
        .to_str()
        .filter(|id| id.len() == 32 && id.is_ascii())
        .ok_or_else(err)?;
    let mut bytes = [0; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&id[index * 2..][..2], 16).map_err(|_| err())?;
    }
    if bytes[0] == 0 {
        return Err(err());
    }
    Ok(bytes)
}
