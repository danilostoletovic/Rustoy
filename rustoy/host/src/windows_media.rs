//! Stage Windows installation files on a new FAT32 EFI partition image.
use crate::install::{format_esp, read_efi, write_loader};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::Path;

const FALLBACK: &str = "efi/boot/bootx64.efi";
const STAGED: &str = "efi/boot/winsetup.efi";

pub fn prepare_windows_esp(output: &OsStr, loader: &OsStr, media: &Path) -> io::Result<()> {
    let application = read_efi(loader)?;
    let mut pending = vec![(media.to_path_buf(), String::new())];
    let mut files = Vec::new();
    let mut names = HashSet::new();
    let mut total = application.len() as u64;
    while let Some((directory, prefix)) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .filter(|name| {
                    !name.is_empty()
                        && name.len() <= 255
                        && name
                            .bytes()
                            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
                        && !name.contains(['\\', ':', '*', '?', '"', '<', '>', '|'])
                        && !name.ends_with([' ', '.'])
                })
                .ok_or_else(|| io::Error::other("Unsupported name in Windows media"))?;
            let relative = format!("{prefix}{}", name.to_ascii_lowercase());
            if relative.len() > 1024 || !names.insert(relative.clone()) || relative == STAGED {
                return Err(io::Error::other(format!(
                    "Conflicting Windows media entry: {relative}"
                )));
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push((entry.path(), format!("{relative}/")));
            } else if kind.is_file() {
                let length = entry.metadata()?.len();
                if length > u64::from(u32::MAX) {
                    return Err(io::Error::other(format!(
                        "{relative} exceeds FAT32's limit. Split an oversized install.wim with DISM before staging."
                    )));
                }
                total = total
                    .checked_add(length)
                    .ok_or_else(|| io::Error::other("Windows media too large"))?;
                files.push((entry.path(), relative, length));
            } else {
                return Err(io::Error::other(
                    "Expected only regular files and directories in Windows media",
                ));
            }
        }
    }
    let contains = |name| {
        files
            .iter()
            .any(|(_, path, length)| path == name && *length > 0)
    };
    if ![
        FALLBACK,
        "efi/microsoft/boot/bcd",
        "boot/boot.sdi",
        "sources/boot.wim",
    ]
    .into_iter()
    .all(contains)
        || ![
            "sources/install.wim",
            "sources/install.esd",
            "sources/install.swm",
        ]
        .into_iter()
        .any(contains)
    {
        return Err(io::Error::other(
            "Expected prepared x64 Windows installation media with an EFI loader, BCD, boot.sdi, boot.wim, and install payload",
        ));
    }
    let boot = files.iter().find(|(_, path, _)| path == FALLBACK).unwrap();
    let windows = read_efi(boot.0.as_os_str())?;
    let mib = 1024 * 1024;
    let length = total
        .checked_add(64 * mib + files.len() as u64 * 32 * 1024)
        .filter(|size| *size <= 32 * 1024 * mib)
        .ok_or_else(|| io::Error::other("Staged Windows media exceeds 32 GiB"))?
        .next_multiple_of(mib);
    let mut image = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(output)?;
    image.set_len(length)?;
    format_esp(&mut image)?;
    image.rewind()?;
    let volume = fatfs::FileSystem::new(&mut image, fatfs::FsOptions::new())?;
    {
        let root = volume.root_dir();
        write_loader(&root, &application)?;
        for (source, relative, length) in files {
            if relative == FALLBACK {
                root.create_file(STAGED)?.write_all(&windows)?;
            } else {
                copy_file(&root, &source, &relative, length)?;
            }
        }
    }
    volume.unmount()?;
    image.sync_all()
}

fn copy_file<T: Read + Write + Seek>(
    root: &fatfs::Dir<'_, T>,
    source: &Path,
    relative: &str,
    length: u64,
) -> io::Result<()> {
    let mut directory = root.clone();
    let mut parts = relative.rsplitn(2, '/');
    let name = parts.next().unwrap();
    if let Some(parent) = parts.next() {
        for component in parent.split('/') {
            directory = directory.create_dir(component)?;
        }
    }
    let source = File::open(source)?;
    if io::copy(
        &mut source.take(length + 1),
        &mut directory.create_file(name)?,
    )? != length
    {
        return Err(io::Error::other(format!(
            "Windows media changed while copying {relative}"
        )));
    }
    Ok(())
}
