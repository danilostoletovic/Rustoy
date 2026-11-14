# Rustoy

Rustoy builds and inspects GPT multiboot images with a Rust host CLI and the pinned Ventoy 1.1.18 UEFI boot stack. The hybrid layout uses an exFAT ISO volume followed by the Ventoy EFI partition.

1. Import the [pinned Ventoy runtime](boot/ventoy/README.md) with `pwsh -File rustoy/scripts/import-ventoy.ps1`, or extract the verified upstream archive on Linux.
2. Follow the [hybrid build and verification instructions](docs/HYBRID.md) to assemble a new disk image and inspect it before testing in QEMU.
3. Run `cargo check --locked`, `cargo test --locked` and `cargo clippy --locked --all-targets` from the repository root.

The hybrid workflow builds regular disk image files. Physical hybrid installation and OS compatibility verification remain in progress. The Windows desktop installer creates native Rustoy media. Native boot experiments are preserved in [experiments](experiments), with separate Cargo targets outside the default build. The root upstream README and licenses remain available.

## Desktop releases

Pushing a tag such as `v0.1.0` on a commit containing the [release workflow](../.github/workflows/rustoy-release.yml) builds both x86-64 packages and publishes them with `SHA256SUMS.txt` in GitHub Releases.

- **Windows** — Run `Rustoy-windows-x86_64.exe` as administrator to install Rustoy to USB. The executable embeds the unsigned native UEFI loader. You can also inspect existing disk images.
- **Linux** — Run `chmod +x Rustoy-linux-x86_64.AppImage`, then launch it to inspect GPT, exFAT and ISO entries in a disk image. The AppImage targets Ubuntu 22.04-era or newer desktop systems with OpenGL. If FUSE is unavailable, launch with `--appimage-extract-and-run`.
