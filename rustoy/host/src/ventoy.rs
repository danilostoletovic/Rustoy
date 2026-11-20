use crate::{
    install::{parse_id, write_image},
    open_image,
};
use rustoy_fs::{exfat, gpt};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{self, Cursor, Read, Seek, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

const ESP_BYTES: usize = 32 * 1024 * 1024;
const RELEASE: &str = "ventoy-1.1.18";
const ZIP: &str = "ventoy-1.1.18-windows.zip";
const ZIP_SHA256: &str = "082c478e8c432e1ac653899fcf50e3a4077a2851bc6112b5ddd30545962164a9";
const ZIP_LIMIT: u64 = 64 * 1024 * 1024;

pub fn import_ventoy(output: &Path, local_archive: Option<&Path>) -> io::Result<PathBuf> {
    if output.symlink_metadata().is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Import destination already exists",
        ));
    }
    let mut bytes = Vec::new();
    if let Some(path) = local_archive {
        let image = open_image(path.as_os_str()).map_err(io::Error::other)?;
        image.file.take(ZIP_LIMIT + 1).read_to_end(&mut bytes)?;
    } else {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .tls_config(
                TlsConfig::builder()
                    .provider(TlsProvider::NativeTls)
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .timeout_global(Some(Duration::from_secs(180)))
            .build();
        let agent = ureq::Agent::new_with_config(config);
        agent
            .get(format!(
                "https://github.com/ventoy/Ventoy/releases/download/v1.1.18/{ZIP}"
            ))
            .call()
            .map_err(io::Error::other)?
            .body_mut()
            .as_reader()
            .take(ZIP_LIMIT + 1)
            .read_to_end(&mut bytes)?;
    }
    if bytes.len() as u64 > ZIP_LIMIT || format!("{:x}", Sha256::digest(&bytes)) != ZIP_SHA256 {
        return Err(io::Error::other("Ventoy archive SHA-256 mismatch"));
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(&bytes))?;
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir(output)?;
    zip.extract(output)?;
    fs::write(output.join(ZIP), &bytes)?;
    Ok(output.join(RELEASE))
}

fn verified(path: &Path, size: usize, expected: &str) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(size as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() != size || format!("{:x}", Sha256::digest(&bytes)) != expected {
        return Err(io::Error::other(format!(
            "Unexpected Ventoy 1.1.18 artifact: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

pub fn assemble_ventoy_image(
    output: &OsStr,
    data: &OsStr,
    release: &Path,
    id: &OsStr,
) -> io::Result<()> {
    let id = parse_id(id)?;
    let mbr = verified(
        &release.join("boot/boot.img"),
        512,
        "85b4b080765738ca96620c32d8182edddbaf6a36149162946d12aa1f9802bc44",
    )?;
    let archive = verified(
        &release.join("ventoy/ventoy.disk.img.xz"),
        13_434_140,
        "19174f133c4df8acda4423b1bc40ae69961773ef54e286ffd8c42fefaf6f174f",
    )?;
    let mut esp = Vec::with_capacity(ESP_BYTES);
    lzma_rust2::XzReader::new(archive.as_slice(), false)
        .take(ESP_BYTES as u64 + 1)
        .read_to_end(&mut esp)?;
    if esp.len() != ESP_BYTES {
        return Err(io::Error::other("Unexpected VTOYEFI size"));
    }
    {
        let fs = fatfs::FileSystem::new(Cursor::new(esp.as_mut_slice()), fatfs::FsOptions::new())?;
        if fs.read_volume_label_from_root_dir()?.as_deref() != Some("VTOYEFI") {
            return Err(io::Error::other("Missing VTOYEFI volume label"));
        }
        {
            let root = fs.root_dir();
            for required in [
                "grub/grub.cfg",
                "ventoy/ventoy.cpio",
                "ventoy/ventoy_x64.efi",
            ] {
                root.open_file(required)?;
            }
            let boot = root.open_dir("EFI/BOOT")?;
            let mut grub = Vec::new();
            boot.open_file("grubx64_real.efi")?
                .take(8 * 1024 * 1024)
                .read_to_end(&mut grub)?;
            if grub.len() < 512 || !grub.starts_with(b"MZ") {
                return Err(io::Error::other(
                    "The packaged x64 GRUB EFI binary is invalid",
                ));
            }
            // Match the upstream no-shim entry point on this test image.
            // Never enter the shim path that enables Ventoy's default bypass.
            let mut entry = boot.open_file("BOOTX64.EFI")?;
            entry.truncate()?;
            entry.write_all(&grub)?;
            entry.flush()?;
        }
        fs.unmount()?;
    }
    let mut data = open_image(data).map_err(io::Error::other)?;
    let length = data.length;
    if !length.is_multiple_of(512) {
        return Err(io::Error::other("Unaligned exFAT data image"));
    }
    let geometry = exfat::read_geometry(&mut data, 0, length)
        .map_err(|err| io::Error::other(format!("Invalid exFAT partition: {err:?}")))?;
    if geometry.sector_size != 512 {
        return Err(io::Error::other(
            "Ventoy layout requires 512-byte logical sectors",
        ));
    }
    data.file.rewind()?;
    let mut mbr: [u8; 512] = mbr.try_into().unwrap();
    mbr[92] = 0x22;
    mbr[384..400].copy_from_slice(&id);
    mbr[440..444].copy_from_slice(&id[12..]);
    write_image(
        output,
        id,
        mbr,
        [
            (gpt::BASIC_DATA, 2048, length, &mut data.file),
            (
                gpt::EFI_SYSTEM,
                2048 + length / 512,
                esp.len() as u64,
                &mut esp.as_slice(),
            ),
        ],
    )
}
