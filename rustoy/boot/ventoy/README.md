# Ventoy boot runtime

Rustoy consumes the official Ventoy 1.1.18 Windows release from upstream revision `6116894`. Its SHA-256 is `082c478e8c432e1ac653899fcf50e3a4077a2851bc6112b5ddd30545962164a9`. See the [release and matching source](https://github.com/ventoy/Ventoy/releases/tag/v1.1.18). The local reference checkout is `74cff34240dca6991b79c6656770670da7b0c39e`; rebuilds of the packaged artifacts should use the release revision.

| Release path | SHA-256 |
| --- | --- |
| `boot/boot.img` | `85b4b080765738ca96620c32d8182edddbaf6a36149162946d12aa1f9802bc44` |
| `ventoy/ventoy.disk.img.xz` | `19174f133c4df8acda4423b1bc40ae69961773ef54e286ffd8c42fefaf6f174f` |

Keep the EFI partition image intact as a dependency unit. It contains mutually dependent GRUB and EFI binaries, GRUB configuration, ISO/UDF drivers, Linux hooks, WinPE helpers, and shim/MOK files. The assembler selects the packaged GRUB executable as the fallback EFI entry for the current UEFI test profile. The shim files are retained for future trust-chain qualification.

To update, review the new release and matching source, obtain the published archive checksum independently, and update the importer and Rust artifact pins together. Run the GPT verification, ISO checksum read-back, OVMF menu and Linux/Windows guest tests before accepting new pins. Test Secure Boot separately with a private OVMF variable store.

The [upstream license collection](../../../License) and [GPL text](../../../COPYING) remain in the checkout. Distribution of generated images or runtime binaries must include applicable upstream notices and corresponding source for the distributed release. The importer keeps the archive local under `target/`.
