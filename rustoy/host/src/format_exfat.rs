//! Build a single-FAT exFAT partition image containing contiguous ISO files.
use crate::{Image, open_image};
use rustoy_fs::exfat;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const SECTOR: u64 = 512;
const CLUSTER: u64 = 32 * 1024;
const FAT_SECTOR: u64 = 24;
const FREE_CLUSTERS: u64 = 1024;

struct IsoFile {
    name: String,
    image: Image,
}

pub fn format_exfat(output: &OsStr, paths: &[OsString]) -> io::Result<()> {
    let mut names = HashSet::new();
    let mut files = Vec::new();
    let mut file_clusters = 0;
    let mut directory_bytes = 4 * 32; // label, bitmap, up-case table, end marker
    for path in paths {
        let name = Path::new(path)
            .file_name()
            .and_then(OsStr::to_str)
            .filter(|name| {
                name.len() > 4
                    && name.len() <= 255
                    && name.to_ascii_lowercase().ends_with(".iso")
                    && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            })
            .ok_or_else(|| io::Error::other("Use .iso filenames containing ASCII letters, digits, dots, underscores or hyphens"))?;
        if !names.insert(name.to_ascii_uppercase()) {
            return Err(io::Error::other(format!("Duplicate ISO file name: {name}")));
        }
        let image = open_image(path).map_err(io::Error::other)?;
        if image.length == 0 {
            return Err(io::Error::other(format!("Empty ISO file: {name}")));
        }
        file_clusters += image.length.div_ceil(CLUSTER);
        directory_bytes += (2 + name.len().div_ceil(15)) * 32;
        if directory_bytes > 16_384 * 32 {
            return Err(io::Error::other("ISO directory is too large"));
        }
        files.push(IsoFile {
            name: name.into(),
            image,
        });
    }
    let upcase = upcase_table();
    let upcase_clusters = (upcase.len() as u64).div_ceil(CLUSTER);
    let root_clusters = (directory_bytes as u64).div_ceil(CLUSTER);
    let base = root_clusters + upcase_clusters + file_clusters + FREE_CLUSTERS;
    let mut bitmap_clusters = 1;
    while (base + bitmap_clusters).div_ceil(8 * CLUSTER) > bitmap_clusters {
        bitmap_clusters = (base + bitmap_clusters).div_ceil(8 * CLUSTER);
    }
    let clusters = base + bitmap_clusters;
    if clusters > 0xffff_fff5 {
        return Err(io::Error::other("Output exceeds exFAT's cluster limit"));
    }
    let used = clusters - FREE_CLUSTERS;
    let fat_sectors = ((clusters + 2) * 4).div_ceil(SECTOR);
    let heap = (FAT_SECTOR + fat_sectors).next_multiple_of(CLUSTER / SECTOR);
    let sectors = heap + clusters * (CLUSTER / SECTOR);
    let bitmap = 2 + root_clusters;
    let upcase_start = bitmap + bitmap_clusters;
    let position = |cluster| heap * SECTOR + (cluster - 2) * CLUSTER;

    let mut image = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(output)?;
    image.set_len(sectors * SECTOR)?;
    let boot = boot_region(sectors, fat_sectors, heap, clusters, used);
    image.write_all(&boot)?;
    image.write_all(&boot)?;

    image.seek(SeekFrom::Start(FAT_SECTOR * SECTOR))?;
    image.write_all(&0xffff_fff8u32.to_le_bytes())?;
    image.write_all(&u32::MAX.to_le_bytes())?;
    for range in [
        2..bitmap,
        bitmap..upcase_start,
        upcase_start..upcase_start + upcase_clusters,
    ] {
        for cluster in range.clone() {
            let next = if cluster + 1 == range.end {
                u32::MAX
            } else {
                cluster as u32 + 1
            };
            image.write_all(&next.to_le_bytes())?;
        }
    }

    image.seek(SeekFrom::Start(position(bitmap)))?;
    io::copy(&mut io::repeat(0xff).take(used / 8), &mut image)?;
    if used % 8 != 0 {
        image.write_all(&[((1u16 << (used % 8)) - 1) as u8])?;
    }
    image.seek(SeekFrom::Start(position(upcase_start)))?;
    image.write_all(&upcase)?;

    let mut directory = Vec::with_capacity(directory_bytes);
    let mut label = [0; 32];
    label[..2].copy_from_slice(&[0x83, 6]);
    for (bytes, letter) in label[2..14].chunks_exact_mut(2).zip(b"RUSTOY") {
        bytes[0] = *letter;
    }
    directory.extend_from_slice(&label);
    directory.extend_from_slice(&metadata_entry(0x81, bitmap, clusters.div_ceil(8), 0));
    directory.extend_from_slice(&metadata_entry(
        0x82,
        upcase_start,
        upcase.len() as u64,
        checksum(&upcase),
    ));
    let mut first = upcase_start + upcase_clusters;
    for mut file in files {
        add_file_entries(&mut directory, &file.name, first as u32, file.image.length);
        image.seek(SeekFrom::Start(position(first)))?;
        let length = file.image.length;
        if io::copy(&mut (&mut file.image.file).take(length), &mut image)? != length {
            return Err(io::Error::other(format!(
                "ISO was truncated while copying {}",
                file.name
            )));
        }
        first += length.div_ceil(CLUSTER);
    }
    directory.extend_from_slice(&[0; 32]);
    image.seek(SeekFrom::Start(position(2)))?;
    image.write_all(&directory)?;
    image.sync_all()?;
    verify_directory(image, sectors * SECTOR, &names)
}

fn checksum(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |sum, byte| {
        sum.rotate_right(1).wrapping_add(u32::from(*byte))
    })
}

fn boot_region(sectors: u64, fat: u64, heap: u64, clusters: u64, used: u64) -> [u8; 6144] {
    let mut boot = [0; 6144];
    boot[..11].copy_from_slice(b"\xeb\x76\x90EXFAT   ");
    boot[72..80].copy_from_slice(&sectors.to_le_bytes());
    let serial = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u32;
    for (offset, value) in [
        (80, FAT_SECTOR as u32),
        (84, fat as u32),
        (88, heap as u32),
        (92, clusters as u32),
        (96, 2),
        (100, serial),
    ] {
        boot[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    boot[104..113].copy_from_slice(&[0, 1, 0, 0, 9, 6, 1, 0x80, (used * 100 / clusters) as u8]);
    boot[120..510].fill(0xf4);
    for sector in 0..=8 {
        let end = (sector + 1) * SECTOR as usize;
        boot[end - 2..end].copy_from_slice(&[0x55, 0xaa]);
    }
    let sum = boot[..11 * SECTOR as usize]
        .iter()
        .enumerate()
        .filter(|(index, _)| !matches!(index, 106 | 107 | 112))
        .fold(0u32, |sum, (_, byte)| {
            sum.rotate_right(1).wrapping_add(u32::from(*byte))
        });
    for word in boot[11 * SECTOR as usize..].chunks_exact_mut(4) {
        word.copy_from_slice(&sum.to_le_bytes());
    }
    boot
}

fn upcase_table() -> Vec<u8> {
    fn uppercase(code: u16) -> u16 {
        if let Some(ch) = char::from_u32(u32::from(code)) {
            let mut mapped = ch.to_uppercase();
            if let Some(upper) = mapped.next().and_then(|upper| u16::try_from(upper).ok())
                && mapped.next().is_none()
            {
                return upper;
            }
        }
        code
    }
    let mut table = Vec::new();
    let mut code = 0;
    while code <= u32::from(u16::MAX) {
        let upper = uppercase(code as u16);
        if upper == code as u16 {
            let start = code;
            while code <= u32::from(u16::MAX)
                && code - start < u32::from(u16::MAX)
                && uppercase(code as u16) == code as u16
            {
                code += 1;
            }
            table.extend_from_slice(&u16::MAX.to_le_bytes());
            table.extend_from_slice(&((code - start) as u16).to_le_bytes());
        } else {
            table.extend_from_slice(&upper.to_le_bytes());
            code += 1;
        }
    }
    table
}

fn metadata_entry(kind: u8, first: u64, length: u64, sum: u32) -> [u8; 32] {
    let mut entry = [0; 32];
    entry[0] = kind;
    entry[4..8].copy_from_slice(&sum.to_le_bytes());
    entry[20..24].copy_from_slice(&(first as u32).to_le_bytes());
    entry[24..32].copy_from_slice(&length.to_le_bytes());
    entry
}

fn add_file_entries(directory: &mut Vec<u8>, name: &str, first: u32, length: u64) {
    let start = directory.len();
    let mut file = [0; 32];
    file[..2].copy_from_slice(&[0x85, 1 + name.len().div_ceil(15) as u8]);
    file[4] = 0x20; // archive
    // 1980-01-01 is the earliest representable file date.
    for offset in [8, 12, 16] {
        file[offset..offset + 4].copy_from_slice(&0x0021_0000u32.to_le_bytes());
    }
    directory.extend_from_slice(&file);
    let mut stream = [0; 32];
    stream[..4].copy_from_slice(&[0xc0, 3, 0, name.len() as u8]);
    let hash = name.bytes().fold(0u16, |sum, byte| {
        sum.rotate_right(1)
            .wrapping_add(u16::from(byte.to_ascii_uppercase()))
            .rotate_right(1)
    });
    stream[4..6].copy_from_slice(&hash.to_le_bytes());
    stream[8..16].copy_from_slice(&length.to_le_bytes());
    stream[20..24].copy_from_slice(&first.to_le_bytes());
    stream[24..32].copy_from_slice(&length.to_le_bytes());
    directory.extend_from_slice(&stream);
    for slice in name.as_bytes().chunks(15) {
        let mut filename = [0; 32];
        filename[0] = 0xc1;
        for (bytes, byte) in filename[2..].chunks_exact_mut(2).zip(slice) {
            bytes[0] = *byte;
        }
        directory.extend_from_slice(&filename);
    }
    let sum = directory[start..]
        .iter()
        .enumerate()
        .filter(|(index, _)| !matches!(index, 2 | 3))
        .fold(0u16, |sum, (_, byte)| {
            sum.rotate_right(1).wrapping_add(u16::from(*byte))
        });
    directory[start + 2..start + 4].copy_from_slice(&sum.to_le_bytes());
}

fn verify_directory(file: File, length: u64, expected: &HashSet<String>) -> io::Result<()> {
    let error = |err| io::Error::other(format!("exFAT read-back failed: {err:?}"));
    let mut volume = exfat::Volume::open(Image { file, length }, 0, length).map_err(error)?;
    let mut names = HashSet::new();
    volume
        .visit_root(|entry| {
            names.insert(String::from_utf16_lossy(entry.name()).to_ascii_uppercase());
            true
        })
        .map_err(error)?;
    if names != *expected {
        return Err(io::Error::other(
            "The exFAT directory differs from the requested files",
        ));
    }
    Ok(())
}
