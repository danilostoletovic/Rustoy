# Rustoy implementation status

## Windows desktop app

Run `target/release/rustoy-desktop.exe` as administrator on Windows x64. Choose a USB drive and confirm erasure. After setup, copy ISOs to the root of the RUSTOY volume. The fixed GPT layout has a 256 MiB FAT32 EFI System Partition and one exFAT partition for ISO storage. The installer uses Windows Storage cmdlets to partition and format the selected USB and verifies the hash of the installed EFI loader. The CLI retains its disposable image operations.

To rebuild the USB installer, first build `rustoy-uefi` for `x86_64-unknown-uefi`, set `RUSTOY_EFI_IMAGE` to the resulting EFI file's absolute path, then run `cargo build --locked --release -p rustoy-host --bin rustoy-desktop`. The current payload is unsigned. Booting an ISO from the menu remains under development.

The Windows x64 GNU release build and launch smoke test passed. Windows Storage command signatures and PowerShell syntax were checked. Storage enumeration returned a CIM access error in the test environment, so physical installation remains unverified. No physical disk was formatted.

The [technical design](DESIGN.md) defines the target architecture. This file records implementation and test evidence.

| Component | Result |
| --- | --- |
| Native x86_64 EFI application | Release build passed. OVMF ran the application from an emulated USB device and logged `RUSTOY_EFI_STARTED`. |
| FAT discovery and menu | OVMF listed `fixture-0.iso` and `fixture-1.iso` and logged `RUSTOY_ISO_COUNT=2`. These small filename fixtures exercise discovery. Keyboard events and the ISO probe action require separate VM tests. |
| ISO9660 and El Torito | Three host regression tests passed using synthetic volume fixtures. The reader supports single-extent primary-volume files and initial EFI boot entries in a single catalog sector. Rock Ridge, Joliet, multi-extent files and UDF are pending. |
| Disk layout | `rustoy-host inspect-disk <file.img> [4096]` opens a regular image read-only, validates both GPT copies, and lists root-level ISO filenames from single-FAT exFAT Basic Data partitions. The shared exFAT reader supports contiguous and FAT-chained file reads. Filesystem formatting, the disposable installer, and UEFI discovery on exFAT remain pending. |
| Debian Live | The EFI inspector can look up the kernel and initramfs and check the Linux boot signature. No ISO has booted. |
| Windows optical media | The experimental F6 action publishes read-only UEFI Block I/O views of the ISO and its embedded FAT boot filesystem. It uses firmware LoadImage and StartImage and requires an explicit firmware SecureBoot value of zero while the authenticated data path is pending. The current OVMF tests have not confirmed a child EFI start. Full UDF access, WinPE media reattachment and Setup remain pending. |
| Secure Boot and factory keys | Pending. |

The Windows implementation includes a bounded single-part boot WIM header reader, a header writer for already-encoded replacement lookup and metadata, and a read-only sparse overlay that can alias source ranges and apply byte overrides. The EFI inspector reports the boot index when `/sources/boot.wim` is accessible through ISO9660. The overlay and header writer are library components and are not wired into the EFI optical device. Encoding updated WIM metadata and lookup records, UDF directory redirection and automatic launcher injection remain pending.

The ISO9660 replacement view now redirects `/sources/boot.wim` to a supplied WIM appended virtually at the end of the image and updates the primary volume size. The regression fixture reads back that replacement through the ISO parser and verifies that the original still points to its original WIM. Supplementary ISO9660 volumes and recognized UDF hybrid media are rejected by this replacement path. Thirteen workspace tests and the EFI release build passed. No guest boot was attempted for this increment.

The experimental F6 action now looks for an exact companion filename (for example, `windows.iso.wim` beside `windows.iso`) and passes the replacement ISO view to the EFI optical boot path. An invalid companion fails selection rather than falling back to the original WIM. The OVMF FAT fixture reached the replacement selection marker and the source ISO retained its SHA-256. The firmware did not expose a `SecureBoot` value for that run, so the path failed closed before device publication. See `target/optical-sidecar-3/serial.log`. exFAT companion selection, EFI child startup and Windows guest boot were not verified in this run.

`cargo build --locked --release -p rustoy-windows --bin rustoy-winpe` builds a standalone launcher. It takes an explicit local ISO path, attaches it read-only using Windows Virtual Disk APIs, matches the new drive letter by storage device identity, and starts Setup only from that mount after finding the install payload. Windows host smoke tests confirmed that it rejected missing arguments and attached a synthetic ISO before rejecting it for missing install media. WinPE provider availability, automatic source discovery, and booting real Windows Setup remain unverified. Twelve workspace tests and the x86_64 EFI release build passed.

## Test commands

```text
cargo fmt --all
cargo +stable-x86_64-pc-windows-gnu test --workspace --locked
cargo +stable-x86_64-pc-windows-gnu build --locked --release -p rustoy-uefi --target x86_64-unknown-uefi
cargo +stable-x86_64-pc-windows-gnu run --locked --release -p rustoy-integration -- <qemu> <ovmf-code> <ovmf-vars> <efi-app> <new-output-dir> <iso-a> <iso-b>
```

The Windows MSVC toolchain initially failed because `link.exe` was missing. Windows Application Control blocked some subprocesses inside the default sandbox. The GNU host toolchain and approved execution completed the build and tests. The smoke run used QEMU 10.2.50 with bundled `edk2-x86_64-code.fd` and a disposable copy of `edk2-i386-vars.fd`. Logs are under `target/ovmf-smoke-1/` and `target/ovmf-discovery-1/`. QEMU exported the staged FAT directory read-only. The test did not use physical disks, enrolled keys or any OS payload.

The disk inspection increment passed six workspace tests and rebuilt the release EFI binary. The new tests use in-memory images and cover matching primary and backup GPT tables, corruption in either copy, and exFAT checksum and partition bounds. OVMF was not rerun for this increment.

The exFAT file-read increment passed seven workspace tests and rebuilt the release EFI binary. An in-memory fixture verifies fragmented reads across a cluster boundary, backward seeks, zero-filled uninitialized bytes and rejection of a cyclic chain. The initial build caught an ambiguous integer type in the directory scan limit. The corrected build passed. No OVMF run or guest boot was performed for this increment.

## GPT USB discovery

`rustoy-host assemble-image <new.img> <fat32.img> <exfat.img> <disk-id-hex>` writes a protective MBR, two GPT copies, and the supplied partition bytes to a new regular file. The FAT32 input must already contain `EFI/BOOT/BOOTX64.EFI`. Both inputs must already be formatted. The disk ID is 32 hexadecimal digits in on-disk byte order and should be unique for each independent image.

The host CLI and OVMF both discovered `test-0.iso` and `test-1.iso` in the exFAT partition of a disposable GPT image. They were tiny synthetic discovery fixtures. The final serial log is `target/ovmf-gpt-3/serial.log`. Two earlier runs reached EFI but reported `NotFound` while resolving the backing disk. Using non-exclusive firmware protocol access resolved the failure. The test used a read-only emulated USB device and a private copy of OVMF variables.

```text
cargo test --workspace --locked
cargo build --locked --release -p rustoy-uefi --target x86_64-unknown-uefi
rustoy-host inspect-disk <disk.img>
rustoy-integration <qemu> <ovmf-code> <ovmf-vars> <disk.img> <new-output-dir> --count 2
```

Native volume formatting, menu key automation and real guest boot remain pending. The only first-party unsafe block borrows firmware device path and Block I/O interfaces for the synchronous disk access path. It does not hold those interfaces across input or child image execution.
