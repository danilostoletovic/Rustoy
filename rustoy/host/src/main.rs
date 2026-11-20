#![forbid(unsafe_code)]

use rustoy_fs::Error;
use rustoy_host::{assemble_image, open_image};
use rustoy_iso::Iso;
use std::path::Path;
use std::process::ExitCode;

fn inspect(path: &std::ffi::OsStr) -> Result<(), String> {
    let mut iso =
        Iso::open(open_image(path)?).map_err(|err| format!("ISO9660 probe failed: {err:?}"))?;
    println!("ISO9660 primary volume");
    match iso.efi_initial_image() {
        Ok(extent) => println!(
            "El Torito EFI initial load: {} bytes at {}",
            extent.length, extent.offset
        ),
        Err(Error::NotFound) => println!("No El Torito EFI entry"),
        Err(err) => return Err(format!("cannot inspect El Torito catalog: {err:?}")),
    }
    for path in ["/live/vmlinuz", "/live/initrd.img", "/sources/boot.wim"] {
        match iso.file(path) {
            Ok(extent) => println!("{path}: {} bytes", extent.length),
            Err(Error::NotFound) => {}
            Err(err) => return Err(format!("cannot inspect {path}: {err:?}")),
        }
    }
    Ok(())
}

fn inspect_disk(path: &std::ffi::OsStr, block_size: usize) -> Result<(), String> {
    println!("{}", rustoy_host::inspect_disk(path, block_size)?);
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = match args.as_slice() {
        [command, path] if command == "inspect-iso" => inspect(path),
        [command, path] if command == "inspect-disk" => inspect_disk(path, 512),
        [command, path, sector] if command == "inspect-disk" && sector == "4096" => {
            inspect_disk(path, 4096)
        }
        [command, output, esp, data, id] if command == "assemble-image" => {
            assemble_image(output, esp, data, id).map_err(|err| err.to_string())
        }
        [command, output, isos @ ..] if command == "format-exfat" => {
            rustoy_host::format_exfat(output, isos).map_err(|err| err.to_string())
        }
        [command, output, efi, data, id] if command == "assemble-native-image" => {
            rustoy_host::assemble_native_image(output, efi, data, id).map_err(|err| err.to_string())
        }
        [command, output, efi, media] if command == "prepare-windows-esp" => {
            rustoy_host::prepare_windows_esp(output, efi, Path::new(media))
                .map_err(|err| err.to_string())
        }
        #[cfg(windows)]
        [command] if command == "list-usb" => rustoy_host::usb::list().map(|drives| {
            for drive in drives {
                println!(
                    "Disk {}  {}  {} bytes",
                    drive.number, drive.label, drive.size
                );
            }
        }),
        _ => {
            eprintln!("Usage: rustoy-host inspect-iso <file.iso> | inspect-disk <file.img> [4096]");
            eprintln!("       rustoy-host format-exfat <new-exfat.img> [file.iso ...]");
            eprintln!(
                "       rustoy-host prepare-windows-esp <new-fat32.img> <rustoy-uefi.efi> <windows-media-dir>"
            );
            eprintln!(
                "       rustoy-host assemble-image <new.img> <fat32.img> <exfat.img> <disk-id-hex>"
            );
            eprintln!(
                "       rustoy-host assemble-native-image <new.img> <rustoy-uefi.efi> <exfat.img> <disk-id-hex>"
            );
            #[cfg(windows)]
            eprintln!("       rustoy-host list-usb");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("rustoy: {err}");
            ExitCode::FAILURE
        }
    }
}
