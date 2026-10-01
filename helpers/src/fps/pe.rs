// ------------ Game Image Reader ------------
// Reads the game's PE headers once it is loaded: image size, a fingerprint of this build and where the il2cpp code
// section is. The fingerprint lets the stub remember where it found the frame-rate value last time.

use core::ops::Range;

const DOS_MAGIC: u16 = 0x5A4D;
const NT_SIGNATURE: u32 = 0x0000_4550;
const PE32PLUS_MAGIC: u16 = 0x020B;
const SECTION_HEADER_SIZE: usize = 40;
const HEADER_SPAN: usize = 4096;

const CODE_SECTION: &[u8; 6] = b"il2cpp";

pub struct MappedImage {
    pub size_of_image: usize,
    pub fingerprint: u64,
    pub code_section: Range<usize>,
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let slice = bytes.get(at..at + 2)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at + 4)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

pub unsafe fn parse(base: *const u8) -> Option<MappedImage> {
    let headers = core::slice::from_raw_parts(base, HEADER_SPAN);

    if u16_at(headers, 0)? != DOS_MAGIC {
        return None;
    }
    let nt = u32_at(headers, 0x3C)? as usize;
    if u32_at(headers, nt)? != NT_SIGNATURE {
        return None;
    }

    let coff = nt + 4;
    let section_count = u16_at(headers, coff + 2)? as usize;
    let time_date_stamp = u32_at(headers, coff + 4)?;
    let optional_size = u16_at(headers, coff + 16)? as usize;

    let optional = coff + 20;
    if u16_at(headers, optional)? != PE32PLUS_MAGIC {
        return None;
    }
    let size_of_image = u32_at(headers, optional + 56)?;
    let check_sum = u32_at(headers, optional + 64)?;

    let mut code_section = None;
    let sections = optional + optional_size;
    for index in 0..section_count {
        let header = sections + index * SECTION_HEADER_SIZE;
        let name = headers.get(header..header + 8)?;
        if &name[..6] != CODE_SECTION || name[6] != 0 {
            continue;
        }
        let virtual_size = u32_at(headers, header + 8)? as usize;
        let virtual_address = u32_at(headers, header + 12)? as usize;
        code_section = Some(virtual_address..virtual_address.saturating_add(virtual_size));
        break;
    }

    Some(MappedImage {
        size_of_image: size_of_image as usize,
        fingerprint: crate::fps::shared::fingerprint(time_date_stamp, check_sum, size_of_image),
        code_section: code_section?,
    })
}
