# Rustoy

Rustoy builds and inspects GPT multiboot images with a Rust host CLI. The native x86-64 UEFI loader boots Debian Live from a FAT32 EFI System Partition and a separate exFAT volume containing the ISO files. The Windows desktop app uses Windows Storage APIs directly from Rust to install the native loader to USB.

## Build a native disk image

From the repository root, use an x86-64 Debian Live Standard ISO. ISO basenames may contain ASCII letters, digits, dots, underscores and hyphens. Supply a different nonzero 32-digit hexadecimal disk ID for each independent disk image.

```text
rustup target add x86_64-unknown-uefi
cargo build --locked --release -p rustoy-uefi --target x86_64-unknown-uefi
cargo run --locked -p rustoy-host -- format-exfat data.exfat.img debian-live-13.7.0-amd64-standard.iso
cargo run --locked -p rustoy-host -- assemble-native-image native.img target/x86_64-unknown-uefi/release/rustoy-uefi.efi data.exfat.img 1234567890ab4cde8123456789abcdef
cargo run --locked -p rustoy-host -- inspect-disk native.img
cargo run --locked -p rustoy-integration -- <qemu> <ovmf-secure-code> <ovmf-vars> native.img <new-log-directory> --boot-debian
```

Use an OVMF variable template with Secure Boot off for this unsigned loader. The smoke test presents the disk read-only and uses its own copy of the firmware variables. For the automated Debian boot test, place one ISO on the exFAT volume. See [status](experiments/STATUS.md) for Windows boot and firmware coverage.

Run `cargo test --locked --workspace` to test the Rust workspace.

## Try staged Windows Setup in a disposable image

Use the root of a mounted x64 Windows installation ISO or a directory containing its extracted files. For an `install.wim` larger than FAT32 permits, use DISM on Windows to split it into `install.swm` and its companion files before staging. See the [Microsoft USB installation guide](https://learn.microsoft.com/en-us/windows-hardware/manufacture/desktop/install-windows-from-a-usb-flash-drive).

```text
cargo run --locked -p rustoy-host -- prepare-windows-esp staged-fat32.img target/x86_64-unknown-uefi/release/rustoy-uefi.efi <windows-media-dir>
cargo run --locked -p rustoy-host -- assemble-image staged.img staged-fat32.img data.exfat.img 1234567890ab4cde8123456789abcdef
```

The experimental image stages the Windows files on Rustoy's FAT32 EFI System Partition and offers **F8 Windows Setup**. Linux ISO files stay on the exFAT partition. Completing a Windows installation from this layout is still unverified. The physical installer does not stage Windows media yet.

## Desktop releases

Pushing a tag such as `v0.1.0` on a commit containing the [release workflow](../.github/workflows/rustoy-release.yml) builds both x86-64 packages and publishes them with `SHA256SUMS.txt` in GitHub Releases.

- **Windows** — Run `Rustoy-windows-x86_64.exe` as administrator to install Rustoy to USB. The executable embeds the unsigned native UEFI loader. You can also inspect existing disk images.
- **Linux** — Run `chmod +x Rustoy-linux-x86_64.AppImage`, then launch it to inspect GPT, exFAT and ISO entries in a disk image. The AppImage targets Ubuntu 22.04-era or newer desktop systems with OpenGL. If FUSE is unavailable, launch with `--appimage-extract-and-run`.
