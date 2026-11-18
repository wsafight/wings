use std::fs::File;
use std::io::Read;
use std::path::Path;

use object::Endianness;
use object::read::elf::{FileHeader, ProgramHeader};
use object::read::macho::MachHeader;
use wings_artifact::MAX_LIBRARY_BYTES;

pub(crate) fn read_image_bytes(path: &Path, file_bytes: u64) -> Result<u64, String> {
    let length = usize::try_from(file_bytes).map_err(|_| "module image file size overflow")?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|error| error.to_string())?;
    bytes.resize(length, 0);
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| error.to_string())?;
    let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
        .map_err(|_| "failed to query module image page size")?;
    if !page.is_power_of_two() {
        return Err("unsupported module image page size".into());
    }
    if cfg!(target_os = "macos") {
        macho_image_bytes(&bytes, page)
    } else {
        elf_image_bytes(&bytes, page)
    }
}

#[derive(Default)]
struct ImageSpan {
    start: Option<u64>,
    end: u64,
}

impl ImageSpan {
    fn include(
        &mut self,
        address: u64,
        memory: u64,
        offset: u64,
        file_size: u64,
        length: u64,
        page: u64,
    ) -> Result<(), String> {
        if file_size > memory || offset.checked_add(file_size).is_none_or(|end| end > length) {
            return Err("invalid module image segment file range".into());
        }
        if memory == 0 {
            return Ok(());
        }
        let start = address & !(page - 1);
        let end = address
            .checked_add(memory)
            .and_then(|end| end.checked_add(page - 1))
            .map(|end| end & !(page - 1))
            .ok_or("module image segment address overflow")?;
        self.start = Some(self.start.map_or(start, |previous| previous.min(start)));
        self.end = self.end.max(end);
        Ok(())
    }

    fn bytes(self) -> Result<u64, String> {
        let bytes = self.end - self.start.ok_or("module image has no loadable segments")?;
        if bytes > MAX_LIBRARY_BYTES {
            return Err("module image span exceeds 64 MiB".into());
        }
        Ok(bytes)
    }
}

fn elf_image_bytes(bytes: &[u8], page: u64) -> Result<u64, String> {
    let header =
        object::elf::FileHeader64::<Endianness>::parse(bytes).map_err(|error| error.to_string())?;
    let endian = header.endian().map_err(|error| error.to_string())?;
    if header.e_type(endian) != object::elf::ET_DYN {
        return Err("module image must be an ELF shared library".into());
    }
    let segments = header
        .program_headers(endian, bytes)
        .map_err(|error| error.to_string())?;
    let mut span = ImageSpan::default();
    for segment in segments {
        if segment.p_type(endian) == object::elf::PT_LOAD {
            span.include(
                segment.p_vaddr(endian),
                segment.p_memsz(endian),
                segment.p_offset(endian),
                segment.p_filesz(endian),
                bytes.len() as u64,
                page,
            )?;
        }
    }
    span.bytes()
}

fn macho_image_bytes(bytes: &[u8], page: u64) -> Result<u64, String> {
    if object::FileKind::parse(bytes).map_err(|error| error.to_string())?
        != object::FileKind::MachO64
    {
        return Err("module image must use Mach-O64".into());
    }
    let header = object::macho::MachHeader64::<Endianness>::parse(bytes, 0)
        .map_err(|error| error.to_string())?;
    let endian = header.endian().map_err(|error| error.to_string())?;
    if header.filetype(endian) != object::macho::MH_DYLIB {
        return Err("module image must be a Mach-O shared library".into());
    }
    let mut commands = header
        .load_commands(endian, bytes, 0)
        .map_err(|error| error.to_string())?;
    let mut span = ImageSpan::default();
    while let Some(command) = commands.next().map_err(|error| error.to_string())? {
        if let Some((segment, _)) = command.segment_64().map_err(|error| error.to_string())? {
            span.include(
                segment.vmaddr.get(endian),
                segment.vmsize.get(endian),
                segment.fileoff.get(endian),
                segment.filesize.get(endian),
                bytes.len() as u64,
                page,
            )?;
        }
    }
    span.bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(bytes: &mut [u8], offset: usize, value: u64, width: usize, big: bool) {
        let encoded = if big {
            value.to_be_bytes()
        } else {
            value.to_le_bytes()
        };
        let start = if big { 8 - width } else { 0 };
        bytes[offset..offset + width].copy_from_slice(&encoded[start..start + width]);
    }

    fn fixture(macho: bool, big: bool, segments: &[[u64; 4]]) -> Vec<u8> {
        let mut bytes = vec![0; 1024];
        if macho {
            word(&mut bytes, 0, 0xfeedfacf, 4, big);
            word(&mut bytes, 12, object::macho::MH_DYLIB.into(), 4, big);
            word(&mut bytes, 16, segments.len() as u64, 4, big);
            word(&mut bytes, 20, (segments.len() * 72) as u64, 4, big);
        } else {
            bytes[..7].copy_from_slice(&[0x7f, b'E', b'L', b'F', 2, if big { 2 } else { 1 }, 1]);
            word(&mut bytes, 16, object::elf::ET_DYN.into(), 2, big);
            word(&mut bytes, 32, 64, 8, big);
            word(&mut bytes, 54, 56, 2, big);
            word(&mut bytes, 56, segments.len() as u64, 2, big);
        }
        for (index, &[address, memory, offset, file_size]) in segments.iter().enumerate() {
            if macho {
                let base = 32 + index * 72;
                word(
                    &mut bytes,
                    base,
                    object::macho::LC_SEGMENT_64.into(),
                    4,
                    big,
                );
                word(&mut bytes, base + 4, 72, 4, big);
                for (field, value) in [address, memory, offset, file_size].into_iter().enumerate() {
                    word(&mut bytes, base + 24 + field * 8, value, 8, big);
                }
            } else {
                let base = 64 + index * 56;
                word(&mut bytes, base, object::elf::PT_LOAD.into(), 4, big);
                for (field, value) in [(8, offset), (16, address), (32, file_size), (40, memory)] {
                    word(&mut bytes, base + field, value, 8, big);
                }
            }
        }
        bytes
    }

    fn parse(bytes: &[u8], macho: bool, page: u64) -> Result<u64, String> {
        if macho {
            macho_image_bytes(bytes, page)
        } else {
            elf_image_bytes(bytes, page)
        }
    }

    #[test]
    fn both_formats_cover_zero_fill_holes_overlap_and_native_pages() {
        for macho in [false, true] {
            for big in [false, true] {
                for page in [4096, 16384] {
                    for (segments, expected) in [
                        (vec![[0, page * 3, 0, 1024]], page * 3),
                        (vec![[0, 1, 0, 1], [page * 3, 1, 1, 1]], page * 4),
                        (
                            vec![[page + 1, page, 0, 512], [page * 2, 512, 512, 512]],
                            page * 2,
                        ),
                        (vec![[page, 0, 0, 0], [page + 1, 1, 0, 1]], page),
                        (vec![[0, MAX_LIBRARY_BYTES, 0, 1024]], MAX_LIBRARY_BYTES),
                    ] {
                        assert_eq!(
                            parse(&fixture(macho, big, &segments), macho, page).unwrap(),
                            expected
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn both_formats_reject_invalid_ranges_empty_images_overflow_and_span_limit() {
        for macho in [false, true] {
            for big in [false, true] {
                for segments in [
                    vec![],
                    vec![[0, 0, 0, 0]],
                    vec![[0, 1, 0, 2]],
                    vec![[0, 4096, 1024, 1]],
                    vec![[0, 4096, u64::MAX, 1]],
                    vec![[u64::MAX, 2, 0, 1]],
                    vec![[u64::MAX - 100, 1, 0, 1]],
                    vec![[0, MAX_LIBRARY_BYTES + 1, 0, 1]],
                    vec![[0, 1, 0, 1], [MAX_LIBRARY_BYTES, 1, 1, 1]],
                ] {
                    assert!(parse(&fixture(macho, big, &segments), macho, 4096).is_err());
                }
            }
        }
    }

    #[test]
    fn both_formats_reject_wrong_kind_headers_and_every_truncated_table() {
        for macho in [false, true] {
            for big in [false, true] {
                let mut bytes = fixture(macho, big, &[[0, 4096, 0, 0]]);
                let table_end = if macho { 104 } else { 120 };
                for end in 0..table_end {
                    assert!(
                        parse(&bytes[..end], macho, 4096).is_err(),
                        "{macho} {big} {end}"
                    );
                }
                let kind_offset = if macho { 12 } else { 16 };
                word(&mut bytes, kind_offset, 1, if macho { 4 } else { 2 }, big);
                assert!(parse(&bytes, macho, 4096).is_err());
                bytes[..4].fill(0);
                assert!(parse(&bytes, macho, 4096).is_err());
            }
        }
    }

    #[test]
    fn macho_rejects_malformed_commands_instead_of_accepting_a_partial_span() {
        for big in [false, true] {
            for command_size in [0, 7, 8, 71, 1024] {
                let mut bytes = fixture(true, big, &[[0, 4096, 0, 1024]]);
                word(&mut bytes, 36, command_size, 4, big);
                assert!(macho_image_bytes(&bytes, 4096).is_err());
            }
            let mut bytes = fixture(true, big, &[[0, 4096, 0, 1024]]);
            word(&mut bytes, 16, 2, 4, big);
            assert!(macho_image_bytes(&bytes, 4096).is_err());
            word(&mut bytes, 0, 0xfeedface, 4, big);
            assert!(macho_image_bytes(&bytes, 4096).is_err());
        }
    }
}
