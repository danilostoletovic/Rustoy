fn main() {
    println!("cargo:rerun-if-env-changed=RUSTOY_EFI_IMAGE");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("rustoy.efi");
    if let Some(input) = std::env::var_os("RUSTOY_EFI_IMAGE") {
        println!(
            "cargo:rerun-if-changed={}",
            std::path::Path::new(&input).display()
        );
        std::fs::copy(input, out).expect("copy Rustoy EFI application");
    } else {
        std::fs::write(out, []).expect("create empty EFI resource");
    }
}
