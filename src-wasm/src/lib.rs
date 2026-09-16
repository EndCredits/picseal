mod utils;

use gloo_utils::format::JsValueSerdeExt;
use serde::Serialize;
use wasm_bindgen::prelude::*;

#[cfg(feature = "wee_alloc")]
#[global_allocator]
static ALLOC: wee_alloc::WeeAlloc = wee_alloc::WeeAlloc::INIT;

#[derive(Debug, Serialize)]
struct ExifData {
    tag: String,
    value: String,
    value_with_unit: String,
}

#[derive(Debug, Serialize)]
struct HdrInfo {
    is_hdr: bool,
    kind: String,
}

#[wasm_bindgen(start)]
pub fn run() {
    utils::set_panic_hook();
}

#[wasm_bindgen]
pub fn get_exif(raw: Vec<u8>) -> JsValue {
    let mut exif_data: Vec<ExifData> = Vec::new();
    let exif_reader = exif::Reader::new();
    let mut bufreader = std::io::Cursor::new(raw.as_slice());

    // Try to read EXIF data, fallback to empty if it fails
    match exif_reader.read_from_container(&mut bufreader) {
        Ok(exif) => {
            for field in exif.fields() {
                exif_data.push(ExifData {
                    tag: field.tag.to_string(),
                    value: field.display_value().to_string(),
                    value_with_unit: field.display_value().with_unit(&exif).to_string(),
                });
            }
        }
        Err(_) => {
            // Use empty EXIF data if parsing fails
        }
    }

    <wasm_bindgen::JsValue as JsValueSerdeExt>::from_serde(&exif_data).unwrap()
}

fn u32be(data: &[u8], off: usize) -> Option<usize> {
    if off + 4 <= data.len() {
        Some(u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as usize)
    } else {
        None
    }
}

fn contains(data: &[u8], needle: &[u8]) -> bool {
    needle.len() <= data.len() && data.windows(needle.len()).any(|w| w == needle)
}

// Locate the ISO-BMFF 'meta' box payload range (skips box header + fullbox version/flags)
fn bmff_meta_range(raw: &[u8]) -> Option<(usize, usize)> {
    let mut off = 0usize;
    while off + 8 <= raw.len() {
        let size32 = u32be(raw, off)?;
        let typ = &raw[off + 4..off + 8];
        let (hdr_len, size) = if size32 == 1 {
            if off + 16 > raw.len() {
                return None;
            }
            let hi = u32be(raw, off + 8)? as u64;
            let lo = u32be(raw, off + 12)? as u64;
            (16usize, (hi << 32) + lo)
        } else {
            (8usize, size32 as u64)
        };
        if typ == b"meta" {
            let end = if size == 0 { raw.len() } else { off.saturating_add(size as usize).min(raw.len()) };
            let start = (off + hdr_len + 4).min(end);
            return Some((start, end));
        }
        if size < 8 {
            break;
        }
        let next = off.saturating_add(size as usize);
        if next <= off {
            break;
        }
        off = next;
    }
    None
}

// Scan 'nclx' colour boxes for PQ(16)/HLG(18) transfer characteristics
fn nclx_is_hdr(data: &[u8]) -> bool {
    if data.len() < 10 {
        return false;
    }
    for i in 0..=(data.len() - 10) {
        if &data[i..i + 4] == b"nclx" {
            let transfer = u16::from_be_bytes([data[i + 6], data[i + 7]]);
            if transfer == 16 || transfer == 18 {
                return true;
            }
        }
    }
    false
}

fn detect_hdr_inner(raw: &[u8]) -> HdrInfo {
    let none = HdrInfo { is_hdr: false, kind: String::new() };
    if raw.len() < 16 {
        return none;
    }
    let hdr = |kind: &str| HdrInfo { is_hdr: true, kind: kind.to_string() };

    // PNG: walk chunks before IDAT (cICP PQ/HLG, mDCv, cLLi, gain map XMP)
    if raw.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        let mut off = 8usize;
        while off + 12 <= raw.len() {
            let len = match u32be(raw, off) { Some(l) => l, None => break };
            let typ = &raw[off + 4..off + 8];
            let body_start = off + 8;
            let body_end = body_start + len;
            if body_end + 4 > raw.len() {
                break;
            }
            let body = &raw[body_start..body_end];
            if typ == b"cICP" && len >= 2 && (body[1] == 16 || body[1] == 18) {
                return hdr("png-pq");
            }
            if typ == b"mDCv" || typ == b"cLLi" {
                return hdr("png-static-hdr");
            }
            if contains(body, b"hdrgm:") || contains(body, b"hdr-gain-map") {
                return hdr("png-gainmap");
            }
            if typ == b"IDAT" || typ == b"IEND" {
                break;
            }
            off = body_end + 4; // skip CRC
        }
        return none;
    }

    // JPEG: walk APP segments before SOS (XMP hdrgm / ISO 21496-1 gain map)
    if raw[0] == 0xFF && raw[1] == 0xD8 {
        let mut off = 2usize;
        let limit = raw.len().min(1024 * 1024);
        while off + 4 <= limit {
            if raw[off] != 0xFF {
                break;
            }
            let marker = raw[off + 1];
            if marker == 0xDA || marker == 0xD9 {
                break; // SOS / EOI
            }
            if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
                off += 2;
                continue;
            }
            let seg_len = u16::from_be_bytes([raw[off + 2], raw[off + 3]]) as usize;
            if seg_len < 2 {
                break;
            }
            let seg_end = (off + 2 + seg_len).min(limit);
            if (0xE0..=0xEF).contains(&marker) {
                let seg = &raw[off + 4..seg_end];
                if contains(seg, b"hdrgm:")
                    || contains(seg, b"hdr-gain-map")
                    || contains(seg, b"urn:iso:std:iso:ts:21496:-1")
                {
                    return hdr("jpeg-gainmap");
                }
            }
            off += 2 + seg_len;
        }
        return none;
    }

    // ISO BMFF (HEIC/AVIF): ftyp brands + meta box signals
    if &raw[4..8] == b"ftyp" {
        if let Some(ftyp_size) = u32be(raw, 0) {
            let end = ftyp_size.min(raw.len()).max(16);
            let mut b = 16usize; // skip size/type/major_brand/minor_version
            while b + 4 <= end {
                if &raw[b..b + 4] == b"tmap" {
                    return hdr("iso-21496-1");
                }
                b += 4;
            }
        }
        if let Some((start, end)) = bmff_meta_range(raw) {
            let meta = &raw[start..end];
            if contains(meta, b"hdrgainmap") {
                return hdr("apple-gainmap");
            }
            if contains(meta, b"urn:iso:std:iso:ts:21496:-1") || contains(meta, b"tmap") {
                return hdr("iso-21496-1");
            }
            if nclx_is_hdr(meta) {
                return hdr("bmff-pq");
            }
        }
    }
    none
}

#[wasm_bindgen]
pub fn detect_hdr(raw: Vec<u8>) -> JsValue {
    let info = detect_hdr_inner(raw.as_slice());
    <wasm_bindgen::JsValue as JsValueSerdeExt>::from_serde(&info).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_chunk(typ: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(data.len() as u32).to_be_bytes());
        v.extend_from_slice(typ);
        v.extend_from_slice(data);
        v.extend_from_slice(&0u32.to_be_bytes()); // CRC (unused by detector)
        v
    }

    fn png_with_chunks(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&png_chunk(b"IHDR", &[0; 13]));
        for c in chunks {
            v.extend_from_slice(c);
        }
        v.extend_from_slice(&png_chunk(b"IDAT", &[0; 8]));
        v.extend_from_slice(&png_chunk(b"IEND", &[]));
        v
    }

    #[test]
    fn png_cicp_pq_is_hdr() {
        let png = png_with_chunks(&[png_chunk(b"cICP", &[9, 16, 9, 1])]);
        let info = detect_hdr_inner(&png);
        assert!(info.is_hdr && info.kind == "png-pq");
    }

    #[test]
    fn png_sdr_is_not_hdr() {
        let png = png_with_chunks(&[png_chunk(b"cICP", &[1, 13, 6, 1]), png_chunk(b"iCCP", b"profile\0\0abc")]);
        assert!(!detect_hdr_inner(&png).is_hdr);
    }

    #[test]
    fn png_gainmap_xmp_is_hdr() {
        let mut itxt = b"XML:com.adobe.xmp\0\0\0\0\0".to_vec();
        itxt.extend_from_slice(b"<x:xmpmeta xmlns:hdrgm='http://ns.adobe.com/hdr-gain-map/1.0/' hdrgm:Version='1.0'/>");
        let png = png_with_chunks(&[png_chunk(b"iTXt", &itxt)]);
        let info = detect_hdr_inner(&png);
        assert!(info.is_hdr && info.kind == "png-gainmap");
    }

    #[test]
    fn jpeg_hdrgm_xmp_is_hdr() {
        let xmp_payload = [b"http://ns.adobe.com/xap/1.0/\0".as_slice(), b"<x:xmpmeta><rdf:Description xmlns:hdrgm='http://ns.adobe.com/hdr-gain-map/1.0/'/></x:xmpmeta>"].concat();
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
        jpeg.extend_from_slice(&((xmp_payload.len() + 2) as u16).to_be_bytes());
        jpeg.extend_from_slice(&xmp_payload);
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        let info = detect_hdr_inner(&jpeg);
        assert!(info.is_hdr && info.kind == "jpeg-gainmap");
    }

    #[test]
    fn jpeg_plain_is_not_hdr() {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        jpeg.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        assert!(!detect_hdr_inner(&jpeg).is_hdr);
    }

    #[test]
    fn bmff_tmap_brand_is_hdr() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&36u32.to_be_bytes());
        raw.extend_from_slice(b"ftypheic");
        raw.extend_from_slice(&0u32.to_be_bytes());
        raw.extend_from_slice(b"heictmap");
        // no meta box
        let info = detect_hdr_inner(&raw);
        assert!(info.is_hdr && info.kind == "iso-21496-1");
    }

    #[test]
    fn bmff_apple_gainmap_is_hdr() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&16u32.to_be_bytes());
        raw.extend_from_slice(b"ftypheic");
        raw.extend_from_slice(&0u32.to_be_bytes());
        // meta box (fullbox) containing auxC with Apple gain map URN
        let payload = [b"\0\0\0\0".as_slice(), b"auxC\0\0\0\0urn:com:apple:photo:2020:aux:hdrgainmap\0"].concat();
        let meta_size = (payload.len() + 8) as u32;
        raw.extend_from_slice(&meta_size.to_be_bytes());
        raw.extend_from_slice(b"meta");
        raw.extend_from_slice(&payload);
        let info = detect_hdr_inner(&raw);
        assert!(info.is_hdr && info.kind == "apple-gainmap");
    }

    #[test]
    fn bmff_avif_pq_nclx_is_hdr() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&16u32.to_be_bytes());
        raw.extend_from_slice(b"ftypavif");
        raw.extend_from_slice(&0u32.to_be_bytes());
        let payload = [b"\0\0\0\0".as_slice(), b"colr", b"nclx", &(9u16).to_be_bytes(), &(16u16).to_be_bytes(), &(9u16).to_be_bytes(), &[0x80]].concat();
        let meta_size = (payload.len() + 8) as u32;
        raw.extend_from_slice(&meta_size.to_be_bytes());
        raw.extend_from_slice(b"meta");
        raw.extend_from_slice(&payload);
        let info = detect_hdr_inner(&raw);
        assert!(info.is_hdr && info.kind == "bmff-pq");
    }

    #[test]
    fn truncated_input_is_safe() {
        assert!(!detect_hdr_inner(&[0x89, b'P', b'N']).is_hdr);
        assert!(!detect_hdr_inner(&[]).is_hdr);
        assert!(!detect_hdr_inner(&[0xFF; 64]).is_hdr);
    }
}
