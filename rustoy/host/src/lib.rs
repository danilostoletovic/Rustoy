#![forbid(unsafe_code)]
use rustoy_fs::{Error, ReadAt, exfat, gpt};
use std::{
    ffi::OsStr,
    fmt::Write,
    fs::File,
    io::{Read, Seek, SeekFrom},
};
mod install;
pub use install::assemble as assemble_image;

pub struct Image {
    file: File,
    length: u64,
}

impl ReadAt for Image {
    fn len(&self) -> u64 {
        self.length
    }
    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
        if offset
            .checked_add(buffer.len() as u64)
            .ok_or(Error::Bounds)?
            > self.length
        {
            return Err(Error::Bounds);
        }
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|_| Error::Io)?;
        self.file.read_exact(buffer).map_err(|_| Error::Io)
    }
}

pub fn open_image(path: &OsStr) -> Result<Image, String> {
    let file = File::open(path).map_err(|err| err.to_string())?;
    let metadata = file.metadata().map_err(|err| err.to_string())?;
    if !metadata.is_file() {
        return Err("Expected a regular image file".into());
    }
    Ok(Image {
        file,
        length: metadata.len(),
    })
}

pub fn inspect_disk(path: &OsStr, block_size: usize) -> Result<String, String> {
    let mut image = open_image(path)?;
    let mut table = vec![0; gpt::MAX_TABLE_BYTES];
    let mut slots = vec![
        gpt::Partition {
            slot: 0,
            kind: [0; 16],
            id: [0; 16],
            first_lba: 0,
            last_lba: 0
        };
        table.len() / 128
    ];
    let (_, partitions) = gpt::read_layout(&mut image, block_size, &mut table, &mut slots)
        .map_err(|err| format!("GPT validation failed ({err:?})"))?;
    let mut report = String::from("GPT partition tables verified\n");
    for part in partitions {
        let kind = match part.kind {
            gpt::EFI_SYSTEM => "EFI System",
            gpt::BASIC_DATA => "Basic Data",
            _ => "Other",
        };
        let blocks = part.last_lba - part.first_lba + 1;
        let _ = writeln!(
            report,
            "\n{}  {}  ·  LBA {}–{}",
            part.slot, kind, part.first_lba, part.last_lba
        );
        if part.kind == gpt::BASIC_DATA {
            match exfat::Volume::open(
                &mut image,
                part.first_lba * block_size as u64,
                blocks * block_size as u64,
            ) {
                Ok(mut volume) => {
                    let _ = writeln!(report, "   exFAT");
                    volume
                        .visit_root(|entry| {
                            let name = String::from_utf16_lossy(entry.name());
                            if name
                                .rsplit_once('.')
                                .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("iso"))
                            {
                                let _ = writeln!(report, "   ISO {name:?}");
                            }
                            true
                        })
                        .map_err(|err| format!("Cannot list exFAT root ({err:?})"))?;
                }
                Err(err) => {
                    let _ = writeln!(report, "   exFAT probe failed ({err:?})");
                }
            }
        }
    }
    Ok(report.trim_end().into())
}

#[cfg(windows)]
pub mod usb;

mod ventoy;
pub use ventoy::assemble_ventoy_image;
