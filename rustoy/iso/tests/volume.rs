use rustoy_fs::Error;
use rustoy_iso::{Extent, Iso, SECTOR_SIZE};

fn dual(value: u32) -> [u8; 8] {
    let mut field = [0; 8];
    field[..4].copy_from_slice(&value.to_le_bytes());
    field[4..].copy_from_slice(&value.to_be_bytes());
    field
}

fn descriptor(image: &mut [u8], sector: usize, kind: u8) -> &mut [u8] {
    let block = &mut image[sector * SECTOR_SIZE..][..SECTOR_SIZE];
    block[0] = kind;
    block[1..7].copy_from_slice(b"CD001\x01");
    block
}

fn directory_record(output: &mut [u8], name: &[u8], sector: u32, size: u32, flags: u8) {
    output[0] = (33 + name.len() + usize::from(name.len().is_multiple_of(2))) as u8;
    output[2..10].copy_from_slice(&dual(sector));
    output[10..18].copy_from_slice(&dual(size));
    output[25] = flags;
    output[28..32].copy_from_slice(&[1, 0, 0, 1]);
    output[32] = name.len() as u8;
    output[33..33 + name.len()].copy_from_slice(name);
}

fn fixture() -> Vec<u8> {
    let mut image = vec![0; 24 * SECTOR_SIZE];
    let pvd = descriptor(&mut image, 16, 1);
    pvd[80..88].copy_from_slice(&dual(24));
    pvd[128..132].copy_from_slice(&[0, 8, 8, 0]);
    directory_record(&mut pvd[156..], &[0], 19, SECTOR_SIZE as u32, 2);
    let boot = descriptor(&mut image, 17, 0);
    boot[7..30].copy_from_slice(b"EL TORITO SPECIFICATION");
    boot[71..75].copy_from_slice(&20u32.to_le_bytes());
    descriptor(&mut image, 18, 255);
    directory_record(&mut image[19 * SECTOR_SIZE..], b"KERNEL.;1", 21, 6, 0);
    let catalog = &mut image[20 * SECTOR_SIZE..];
    catalog[0] = 1;
    catalog[30..32].copy_from_slice(&[0x55, 0xaa]);
    catalog[28..30].copy_from_slice(&0u16.wrapping_sub(0xaa56).to_le_bytes());
    catalog[64..68].copy_from_slice(&[0x91, 0xef, 1, 0]);
    catalog[96] = 0x88;
    catalog[102] = 4;
    catalog[104] = 22;
    image[21 * SECTOR_SIZE..][..6].copy_from_slice(b"kernel");
    image
}

#[test]
fn reads_iso_file_and_efi_boot_entry_from_the_same_image() {
    let image = fixture();
    let mut iso = Iso::open(image.as_slice()).unwrap();
    let file = iso.file("/kernel.").unwrap();
    let mut contents = [0; 6];
    iso.read(file, 0, &mut contents).unwrap();
    assert_eq!(&contents, b"kernel");
    assert_eq!(iso.read(file, 1, &mut contents), Err(Error::Bounds));
    assert_eq!(
        iso.efi_initial_image().unwrap(),
        Extent {
            offset: 22 * SECTOR_SIZE as u64,
            length: SECTOR_SIZE as u64
        }
    );
}

#[test]
fn rejects_catalog_checksum_errors_and_out_of_bounds_loads() {
    let mut image = fixture();
    image[20 * SECTOR_SIZE + 28] ^= 1;
    let mut iso = Iso::open(image.as_slice()).unwrap();
    assert_eq!(iso.efi_initial_image(), Err(Error::Invalid));
    let mut image = fixture();
    image[20 * SECTOR_SIZE + 104] = 24;
    let mut iso = Iso::open(image.as_slice()).unwrap();
    assert_eq!(iso.efi_initial_image(), Err(Error::Bounds));
}

#[test]
fn rejects_conflicting_endianness_and_truncated_volumes() {
    let image = fixture();
    assert!(matches!(
        Iso::open(&image[..image.len() - 1]),
        Err(Error::Bounds)
    ));
    let mut image = image;
    image[16 * SECTOR_SIZE + 87] ^= 1;
    assert!(matches!(Iso::open(image.as_slice()), Err(Error::Invalid)));
}
