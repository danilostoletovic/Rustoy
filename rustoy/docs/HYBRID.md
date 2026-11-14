# Hybrid USB images

Rustoy's host CLI manages disk images. The hybrid image uses the upstream Ventoy 1.1.18 x86_64 GRUB EFI binary, GRUB configuration and matching runtime image. Those artifacts supply Ventoy's ISO mapping, Linux initramfs hooks, and Windows EFI and WinPE helpers. Native Rustoy EFI, optical and WinPE implementations remain in the repository as experiments. The Windows desktop installer still creates the original Rustoy layout.

## Build a disposable image

Import or extract the [verified Ventoy 1.1.18 release](../boot/ventoy/README.md). Supply a 512-byte-sector exFAT partition image containing the ISOs. Give each independent USB image a unique disk ID in 32-digit hexadecimal on-disk byte order.

```text
cargo run --locked -p rustoy-host -- assemble-ventoy-image new.img data.exfat.img ventoy-1.1.18 1234567890ab4cde8123456789abcdef
cargo run --locked -p rustoy-host -- inspect-disk new.img
```

The implementation verifies the release's boot sector and compressed EFI volume against pinned SHA-256 values before writing. The new GPT layout places exFAT at LBA 2048 and the 32 MiB VTOYEFI partition immediately after it. It preserves the upstream MBR compatibility bytes and populates the disk identity fields that Ventoy reads. The CLI accepts preformatted exFAT input and creates a new regular output file. Physical Linux USB installation, native exFAT formatting, updates and recovery are pending.

## Secure Boot and upstream attribution

The test image selects the packaged real GRUB binary directly as the removable-media EFI entry. It does not use the upstream shim as its entry point, register verification hooks or alter firmware keys. Firmware can reject the GRUB binary according to its enrolled policy. The upstream Secure Boot design uses a signed third-party shim and MOK enrollment and documents a bypass policy as its default. Rustoy has not qualified that chain, revocation, or factory-key compatibility. The first hybrid validation uses an unenrolled OVMF variable store.

Use the matching upstream [Ventoy 1.1.18 release and source](https://github.com/ventoy/Ventoy/releases/tag/v1.1.18). Keep upstream license notices and supply corresponding source when distributing the runtime or generated images under the applicable licenses. The checkout retains the original Ventoy source and licenses. The boot artifacts are consumed from the extracted release.

## Audit and test status

| Component | Disposition |
| --- | --- |
| Rust GPT, exFAT and ISO readers | Shared host inspection and fixture verification. Existing assembler remains available for the native experiment. |
| Rust Windows host GUI | Existing native installer remains available. Physical hybrid installs are pending. |
| Rust UEFI, Linux loader and Windows WIM/WinPE code | Retained as experimental source. The hybrid image boots using the separate Ventoy stack. |
| Rust security crate | Reserved. No verified custom trust chain. |
| Rust integration runner | Existing disposable-media smoke tests retained. Hybrid validation used QEMU monitor key injection and guest screenshots. |
| Ventoy runtime | Matched GRUB and configuration, EFI ISO/UDF drivers, Linux hooks, and Windows boot helpers from the pinned release. |

The builder rejects an existing destination and invalid exFAT input. A read-back SHA-256 of Debian Live 13.7.0 standard amd64 inside the first image matched the upstream ISO. QEMU/OVMF displayed the Ventoy menu, selected the ISO, and reached its Debian Live GRUB menu. Windows 11 has not been tested.

The post-cleanup image matched its pre-cleanup baseline byte for byte. A fresh OVMF run produced the same menu framebuffer with `debian.iso` visible. The upstream source remains available for rebuilding and for source distribution. The legacy packaging workflow references upstream host and LiveCD trees and remains separate from the Rust image workflow.
