//! Read-side building blocks for remapping a WinPE image without editing the ISO.
use rustoy_fs::{Error, ReadAt};

#[derive(Debug, Eq, PartialEq)]
pub struct Resource {
    pub offset: u64,
    pub stored_size: u64,
    pub original_size: u64,
    pub flags: u8,
}

#[derive(Debug, Eq, PartialEq)]
pub struct BootWim {
    pub boot_index: u32,
    pub lookup: Resource,
    pub metadata: Resource,
}

impl BootWim {
    pub fn read(source: &mut impl ReadAt) -> Result<Self, Error> {
        let mut header = [0; 208];
        if source.len() < header.len() as u64 {
            return Err(Error::Bounds);
        }
        source.read_at(0, &mut header)?;
        let word = |offset| u32::from_le_bytes(header[offset..offset + 4].try_into().unwrap());
        if &header[..8] != b"MSWIM\0\0\0" || word(8) != 208 {
            return Err(Error::Invalid);
        }
        if header[40..44] != [1, 0, 1, 0] {
            return Err(Error::Unsupported);
        }
        let boot_index = word(120);
        if boot_index == 0 || boot_index > word(44) {
            return Err(Error::Invalid);
        }
        let resource = |offset: usize| {
            let field = |at| u64::from_le_bytes(header[at..at + 8].try_into().unwrap());
            let packed = field(offset);
            let resource = Resource {
                offset: field(offset + 8),
                stored_size: packed & 0x00ff_ffff_ffff_ffff,
                original_size: field(offset + 16),
                flags: (packed >> 56) as u8,
            };
            if resource.offset < 208
                || resource.stored_size == 0
                || resource.original_size == 0
                || resource
                    .offset
                    .checked_add(resource.stored_size)
                    .is_none_or(|end| end > source.len())
            {
                return Err(Error::Bounds);
            }
            Ok(resource)
        };
        let lookup = resource(48)?;
        let metadata = resource(96)?;
        if metadata.flags & 2 == 0 {
            return Err(Error::Invalid);
        }
        if lookup.offset < metadata.offset + metadata.stored_size
            && metadata.offset < lookup.offset + lookup.stored_size
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            boot_index,
            lookup,
            metadata,
        })
    }
}

/// Build the header for a WIM whose replacement lookup and boot metadata
/// have already been encoded. Existing payload offsets remain unchanged.
pub fn relocated_header(
    source: &mut impl ReadAt,
    metadata: (u64, u64),
    lookup: (u64, u64),
    virtual_length: u64,
) -> Result<[u8; 208], Error> {
    BootWim::read(source)?;
    let mut header = [0; 208];
    source.read_at(0, &mut header)?;
    let mut previous = source.len();
    for (position, (offset, size), flag) in [(96, metadata, 2u64), (48, lookup, 0)] {
        if offset < previous || size == 0 || size > 0x00ff_ffff_ffff_ffff {
            return Err(Error::Invalid);
        }
        previous = offset.checked_add(size).ok_or(Error::Bounds)?;
        if previous > virtual_length {
            return Err(Error::Bounds);
        }
        header[position..position + 8].copy_from_slice(&(size | (flag << 56)).to_le_bytes());
        header[position + 8..position + 16].copy_from_slice(&offset.to_le_bytes());
        header[position + 16..position + 24].copy_from_slice(&size.to_le_bytes());
    }
    // The old integrity table does not describe this modified experimental view.
    // Require an input without that table instead of removing integrity metadata.
    if header[124..148].iter().any(|&byte| byte != 0) {
        return Err(Error::Unsupported);
    }
    Ok(header)
}

pub struct Patch<'a> {
    pub offset: u64,
    pub bytes: &'a [u8],
}

/// Maps a range in the virtual image onto bytes in the unchanged source.
pub struct Redirect {
    pub offset: u64,
    pub source_offset: u64,
    pub length: u64,
}

/// A view with sorted byte overrides and optional appended content. Appended
/// bytes outside a patch read as zero, allowing padding to an optical sector.
pub struct Overlay<'a, R> {
    source: R,
    patches: &'a [Patch<'a>],
    redirects: &'a [Redirect],
    original_len: u64,
    len: u64,
}

impl<'a, R: ReadAt> Overlay<'a, R> {
    pub fn new(source: R, len: u64, patches: &'a [Patch<'a>]) -> Result<Self, Error> {
        Self::with_redirects(source, len, patches, &[])
    }

    pub fn with_redirects(
        source: R,
        len: u64,
        patches: &'a [Patch<'a>],
        redirects: &'a [Redirect],
    ) -> Result<Self, Error> {
        let original_len = source.len();
        if len < original_len {
            return Err(Error::Bounds);
        }
        let mut end = 0;
        for patch in patches {
            if patch.bytes.is_empty() || patch.offset < end {
                return Err(Error::Invalid);
            }
            end = patch
                .offset
                .checked_add(patch.bytes.len() as u64)
                .ok_or(Error::Bounds)?;
            if end > len {
                return Err(Error::Bounds);
            }
        }
        let mut end = 0;
        for redirect in redirects {
            if redirect.length == 0 || redirect.offset < end {
                return Err(Error::Invalid);
            }
            end = redirect
                .offset
                .checked_add(redirect.length)
                .ok_or(Error::Bounds)?;
            if end > len
                || redirect
                    .source_offset
                    .checked_add(redirect.length)
                    .is_none_or(|end| end > original_len)
            {
                return Err(Error::Bounds);
            }
        }
        Ok(Self {
            source,
            patches,
            redirects,
            original_len,
            len,
        })
    }
}

impl<R: ReadAt> ReadAt for Overlay<'_, R> {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
        let end = offset
            .checked_add(buffer.len() as u64)
            .ok_or(Error::Bounds)?;
        if end > self.len {
            return Err(Error::Bounds);
        }
        let original = (end.min(self.original_len).saturating_sub(offset)) as usize;
        let (base, tail) = buffer.split_at_mut(original);
        if !base.is_empty() {
            self.source.read_at(offset, base)?;
        }
        tail.fill(0);
        let first = self
            .redirects
            .partition_point(|range| range.offset + range.length <= offset);
        for range in self.redirects[first..]
            .iter()
            .take_while(|range| range.offset < end)
        {
            let start = range.offset.max(offset);
            let stop = (range.offset + range.length).min(end);
            self.source.read_at(
                range.source_offset + (start - range.offset),
                &mut buffer[(start - offset) as usize..(stop - offset) as usize],
            )?;
        }
        let first = self
            .patches
            .partition_point(|patch| patch.offset + patch.bytes.len() as u64 <= offset);
        for patch in self.patches[first..]
            .iter()
            .take_while(|patch| patch.offset < end)
        {
            let start = patch.offset.max(offset);
            let stop = (patch.offset + patch.bytes.len() as u64).min(end);
            buffer[(start - offset) as usize..(stop - offset) as usize].copy_from_slice(
                &patch.bytes[(start - patch.offset) as usize..(stop - patch.offset) as usize],
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlays_header_and_appended_data_without_writing_source() {
        let original = *b"abcdefgh";
        let patches = [
            Patch {
                offset: 2,
                bytes: b"XY",
            },
            Patch {
                offset: 9,
                bytes: b"12",
            },
        ];
        let mut view = Overlay::new(original.as_slice(), 12, &patches).unwrap();
        let mut read = [0; 10];
        view.read_at(1, &mut read).unwrap();
        assert_eq!(&read, b"bXYefgh\x0012");
        assert_eq!(view.read_at(3, &mut read), Err(Error::Bounds));
        assert_eq!(&original, b"abcdefgh");
        assert!(matches!(
            Overlay::new(original.as_slice(), 10, &patches),
            Err(Error::Bounds)
        ));
        let overlap = [
            Patch {
                offset: 2,
                bytes: b"XY",
            },
            Patch {
                offset: 3,
                bytes: b"Z",
            },
        ];
        assert!(matches!(
            Overlay::new(original.as_slice(), 8, &overlap),
            Err(Error::Invalid)
        ));
    }

    #[test]
    fn validates_wim_boot_index_and_resource_ranges() {
        let mut image = [0; 288];
        image[..8].copy_from_slice(b"MSWIM\0\0\0");
        image[8..12].copy_from_slice(&208u32.to_le_bytes());
        image[40..44].copy_from_slice(&[1, 0, 1, 0]);
        image[44] = 2;
        image[120] = 2;
        for (position, offset, flags) in [(48, 208u64, 0u8), (96, 224, 2)] {
            image[position] = 16;
            image[position + 7] = flags;
            image[position + 8..position + 16].copy_from_slice(&offset.to_le_bytes());
            image[position + 16] = 16;
        }
        assert_eq!(
            BootWim::read(&mut image.as_slice())
                .unwrap()
                .metadata
                .offset,
            224
        );
        let header = relocated_header(&mut image.as_slice(), (288, 16), (304, 16), 320).unwrap();
        assert_eq!(&header[104..112], &288u64.to_le_bytes());
        assert_eq!(&header[56..64], &304u64.to_le_bytes());
        assert_eq!(
            relocated_header(&mut image.as_slice(), (288, 16), (292, 16), 320),
            Err(Error::Invalid)
        );
        image[124] = 1;
        assert_eq!(
            relocated_header(&mut image.as_slice(), (288, 16), (304, 16), 320),
            Err(Error::Unsupported)
        );
        image[124] = 0;
        image[120] = 3;
        assert_eq!(BootWim::read(&mut image.as_slice()), Err(Error::Invalid));
        image[120] = 2;
        image[104] = 209;
        assert_eq!(BootWim::read(&mut image.as_slice()), Err(Error::Invalid));
    }

    #[test]
    fn patches_take_precedence_over_relocated_source_bytes() {
        let original = b"old-WIM!";
        let ranges = [Redirect {
            offset: 10,
            source_offset: 4,
            length: 4,
        }];
        let patches = [Patch {
            offset: 10,
            bytes: b"N",
        }];
        let mut overlay =
            Overlay::with_redirects(original.as_slice(), 16, &patches, &ranges).unwrap();
        let mut out = [0; 10];
        overlay.read_at(6, &mut out).unwrap();
        assert_eq!(&out, b"M!\0\0NIM!\0\0");
        let bad = [Redirect {
            offset: 10,
            source_offset: 6,
            length: 4,
        }];
        assert!(matches!(
            Overlay::with_redirects(original.as_slice(), 16, &[], &bad),
            Err(Error::Bounds)
        ));
    }
}
