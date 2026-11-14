#![cfg_attr(target_os = "uefi", no_main)]
#![cfg_attr(target_os = "uefi", no_std)]
#![deny(unsafe_code)]

#[cfg(not(target_os = "uefi"))]
fn main() {
    eprintln!("Build this application for x86_64-unknown-uefi");
}

#[cfg(all(target_os = "uefi", not(target_arch = "x86_64")))]
compile_error!("Rustoy supports x86_64 UEFI only");

#[cfg(target_os = "uefi")]
extern crate alloc;
#[cfg(target_os = "uefi")]
#[allow(unsafe_code)]
mod data_disk;
#[cfg(target_os = "uefi")]
#[allow(unsafe_code)]
mod linux_boot;
#[cfg(target_os = "uefi")]
#[allow(unsafe_code)]
mod optical;

#[cfg(target_os = "uefi")]
mod firmware {
    extern crate alloc;

    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    use rustoy_fs::{Error, ReadAt};
    use rustoy_iso::Iso;
    use rustoy_linux::DebianLiveAssets;
    use uefi::CStr16;
    use uefi::prelude::*;
    use uefi::proto::console::text::{Key, ScanCode};
    use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode, RegularFile};

    struct EfiFile(RegularFile, u64);

    impl ReadAt for EfiFile {
        fn len(&self) -> u64 {
            self.1
        }

        fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
            if offset
                .checked_add(buffer.len() as u64)
                .ok_or(Error::Bounds)?
                > self.1
            {
                return Err(Error::Bounds);
            }
            self.0.set_position(offset).map_err(|_| Error::Io)?;
            if self.0.read(buffer).map_err(|_| Error::Io)? != buffer.len() {
                return Err(Error::Io);
            }
            Ok(())
        }
    }

    struct Candidate {
        name: String,
        fat_length: Option<u64>,
    }

    fn iso_name(name: &str) -> bool {
        name.len() <= 128
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b" ._-".contains(&byte))
            && name
                .rsplit_once('.')
                .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("iso"))
    }

    fn scan() -> Result<Vec<Candidate>, Error> {
        if let Some(mut volume) = crate::data_disk::open()? {
            let mut images = Vec::new();
            volume.visit_root(|entry| {
                if let Ok(name) = String::from_utf16(entry.name()) {
                    if iso_name(&name) {
                        images.push(Candidate {
                            name,
                            fat_length: None,
                        });
                    }
                }
                images.len() < 128
            })?;
            images.sort_unstable_by(|a, b| a.name.cmp(&b.name));
            return Ok(images);
        }
        scan_fat().map_err(|_| Error::Io)
    }

    fn scan_fat() -> uefi::Result<Vec<Candidate>> {
        let mut fs = uefi::boot::get_image_file_system(uefi::boot::image_handle())?;
        let mut root = fs.open_volume()?;
        let mut images = Vec::new();
        // Bound both the number of directory reads and the firmware buffer size.
        #[repr(align(8))]
        struct EntryBuffer([u8; 1024]);
        let mut buffer = EntryBuffer([0; 1024]);
        for _ in 0..4096 {
            let Some(info) = root.read_entry(&mut buffer.0).map_err(|err| err.status())? else {
                break;
            };
            if info.attribute().contains(FileAttribute::DIRECTORY) {
                continue;
            }
            let name = info.file_name().to_string();
            if iso_name(&name) {
                images.push(Candidate {
                    name,
                    fat_length: Some(info.file_size()),
                });
                if images.len() == 128 {
                    break;
                }
            }
        }
        images.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        Ok(images)
    }

    fn open_iso<T>(
        candidate: &Candidate,
        use_file: impl FnOnce(&mut dyn ReadAt) -> T,
    ) -> Result<T, Error> {
        let Some(length) = candidate.fat_length else {
            let mut volume = crate::data_disk::open()?.ok_or(Error::NotFound)?;
            let mut found = None;
            volume.visit_root(|entry| {
                if entry
                    .name()
                    .iter()
                    .copied()
                    .eq(candidate.name.encode_utf16())
                {
                    found = Some(entry);
                    return false;
                }
                true
            })?;
            return Ok(use_file(
                &mut volume.open_file(found.ok_or(Error::NotFound)?)?,
            ));
        };
        let mut name = [0; 129];
        let path =
            CStr16::from_str_with_buf(&candidate.name, &mut name).map_err(|_| Error::Invalid)?;
        let mut fs =
            uefi::boot::get_image_file_system(uefi::boot::image_handle()).map_err(|_| Error::Io)?;
        let mut root = fs.open_volume().map_err(|_| Error::Io)?;
        let handle = root
            .open(path, FileMode::Read, FileAttribute::empty())
            .map_err(|_| Error::Io)?;
        let regular = handle.into_regular_file().ok_or(Error::Invalid)?;
        Ok(use_file(&mut EfiFile(regular, length)))
    }

    fn inspect_iso(mut file: impl ReadAt) -> Result<(bool, bool, Option<u64>, Option<u32>), Error> {
        let efi = match rustoy_iso::optical_efi_image(&mut file) {
            Ok(_) => true,
            Err(Error::NotFound) => false,
            Err(err) => return Err(err),
        };
        let boot_fs = rustoy_windows::efi_boot_filesystem(&mut file)
            .ok()
            .map(|range| range.length);
        let (debian, wim_index) = match Iso::open(file) {
            Ok(mut iso) => (
                DebianLiveAssets::locate(&mut iso).is_ok(),
                rustoy_windows::probe_iso9660_boot_wim(&mut iso)
                    .ok()
                    .map(|wim| wim.boot_index),
            ),
            Err(_) => (false, None),
        };
        if !efi && !debian {
            return Err(Error::Unsupported);
        }
        Ok((efi, debian, boot_fs, wim_index))
    }

    // A prepared replacement is optional and is named <image>.iso.wim.
    // Probe and open it on the same volume as the selected ISO.
    fn boot_optical(candidate: &Candidate) -> Result<Result<(), Status>, Error> {
        let companion = alloc::format!("{}.wim", candidate.name);
        if candidate.fat_length.is_none() {
            let mut companion_volume = crate::data_disk::open()?.ok_or(Error::NotFound)?;
            let mut entry = None;
            companion_volume.visit_root(|item| {
                if item.name().iter().copied().eq(companion.encode_utf16()) {
                    entry = Some(item);
                    return false;
                }
                true
            })?;
            let Some(entry) = entry else {
                drop(companion_volume);
                return open_iso(candidate, |iso| crate::optical::start(iso));
            };
            let wim = companion_volume.open_file(entry)?;
            return open_iso(candidate, |iso| boot_replacement(iso, wim, &companion))?;
        }
        let mut name = [0; 134];
        let path = CStr16::from_str_with_buf(&companion, &mut name).map_err(|_| Error::Invalid)?;
        let mut fs =
            uefi::boot::get_image_file_system(uefi::boot::image_handle()).map_err(|_| Error::Io)?;
        let mut root = fs.open_volume().map_err(|_| Error::Io)?;
        let handle = match root.open(path, FileMode::Read, FileAttribute::empty()) {
            Ok(handle) => handle,
            Err(err) if err.status() == Status::NOT_FOUND => {
                drop(root);
                drop(fs);
                return open_iso(candidate, |iso| crate::optical::start(iso));
            }
            Err(_) => return Err(Error::Io),
        };
        let mut file = handle.into_regular_file().ok_or(Error::Invalid)?;
        let length = file
            .get_boxed_info::<FileInfo>()
            .map_err(|_| Error::Io)?
            .file_size();
        let path =
            CStr16::from_str_with_buf(&candidate.name, &mut name).map_err(|_| Error::Invalid)?;
        let iso = root
            .open(path, FileMode::Read, FileAttribute::empty())
            .map_err(|_| Error::Io)?
            .into_regular_file()
            .ok_or(Error::Invalid)?;
        boot_replacement(
            EfiFile(iso, candidate.fat_length.ok_or(Error::Invalid)?),
            EfiFile(file, length),
            &companion,
        )
    }

    fn boot_replacement(
        mut iso: impl ReadAt,
        mut wim: impl ReadAt,
        companion: &str,
    ) -> Result<Result<(), Status>, Error> {
        let plan = rustoy_windows::iso_redirect::Replacement::plan(&mut iso, &mut wim)?;
        let view = plan.view(iso, wim)?;
        uefi::println!("Using prepared WIM replacement: {}", companion);
        Ok(crate::optical::start(view))
    }

    fn draw(images: &[Candidate], selected: usize) {
        let _ = uefi::system::with_stdout(|output| output.clear());
        uefi::println!("Rustoy");
        uefi::println!("Enter  Boot Debian Live   F7  Inspect   F5  Rescan   Esc  Exit");
        uefi::println!("F6  Optical boot (experimental)");
        if images.is_empty() {
            uefi::println!("No ISO files found in the boot volume root.");
        }
        let start = (selected / 10) * 10;
        for (index, image) in images.iter().enumerate().skip(start).take(10) {
            let cursor = if index == selected { ">" } else { " " };
            uefi::println!("{} {}", cursor, image.name);
        }
        uefi::println!("RUSTOY_ISO_COUNT={}", images.len());
    }

    #[entry]
    fn main() -> Status {
        if uefi::helpers::init().is_err() {
            return Status::ABORTED;
        }
        uefi::println!("RUSTOY_EFI_STARTED");
        let mut images = scan().unwrap_or_else(|err| {
            uefi::println!("Unable to read boot volume: {:?}", err);
            Vec::new()
        });
        let mut selected = 0;
        // Keep the initial firmware marker visible in the serial transcript.
        draw(&images, selected);
        loop {
            let result = uefi::system::with_stdin(|input| {
                let Some(event) = input.wait_for_key_event() else {
                    return Err(Status::UNSUPPORTED);
                };
                uefi::boot::wait_for_event(&mut [event]).map_err(|err| err.status())?;
                input.read_key().map_err(|err| err.status())
            });
            match result {
                Ok(Some(Key::Special(ScanCode::ESCAPE))) => {
                    uefi::println!("RUSTOY_EFI_EXIT");
                    return Status::SUCCESS;
                }
                Ok(Some(Key::Special(ScanCode::UP))) => {
                    selected = selected.saturating_sub(1);
                    draw(&images, selected);
                }
                Ok(Some(Key::Special(ScanCode::DOWN))) => {
                    selected = (selected + 1).min(images.len().saturating_sub(1));
                    draw(&images, selected);
                }
                Ok(Some(Key::Special(ScanCode::FUNCTION_5))) => match scan() {
                    Ok(updated) => {
                        images = updated;
                        selected = 0;
                        draw(&images, selected);
                    }
                    Err(err) => uefi::println!("Rescan failed: {:?}", err),
                },
                Ok(Some(Key::Special(ScanCode::FUNCTION_6))) => {
                    if let Some(image) = images.get(selected) {
                        match boot_optical(image) {
                            Ok(Ok(())) => uefi::println!("Optical EFI application returned."),
                            Ok(Err(status)) => uefi::println!("Optical boot failed: {:?}", status),
                            Err(err) => uefi::println!("Cannot open ISO: {:?}", err),
                        }
                    }
                }
                Ok(Some(Key::Printable(key))) if u16::from(key) == 13 => {
                    if let Some(image) = images.get(selected) {
                        match open_iso(image, |file| crate::linux_boot::start(file, &image.name)) {
                            Ok(Ok(())) => uefi::println!("The Linux kernel returned to firmware."),
                            Ok(Err(status)) => {
                                uefi::println!("Debian Live boot failed: {:?}", status)
                            }
                            Err(err) => uefi::println!("Cannot open ISO: {:?}", err),
                        }
                    }
                }
                Ok(Some(Key::Special(ScanCode::FUNCTION_7))) => {
                    if let Some(image) = images.get(selected) {
                        draw(&images, selected);
                        match open_iso(image, |file| inspect_iso(file))
                            .and_then(core::convert::identity)
                        {
                            Ok((efi, debian, boot_fs, wim_index)) => {
                                if let Some(index) = wim_index {
                                    uefi::println!("Windows boot.wim boot index: {}", index);
                                }
                                uefi::println!("El Torito EFI entry: {}", efi);
                                uefi::println!("Debian Live boot assets found: {}", debian);
                                if let Some(length) = boot_fs {
                                    uefi::println!(
                                        "Embedded FAT boot filesystem: {} bytes",
                                        length
                                    );
                                }
                            }
                            Err(err) => uefi::println!("ISO probe failed: {:?}", err),
                        }
                    }
                }
                Ok(_) => {}
                Err(status) => return status,
            }
        }
    }
}
