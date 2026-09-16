mod utils;

use gloo_utils::format::JsValueSerdeExt;
use serde::Serialize;
use wasm_bindgen::prelude::*;

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

// Parse an MPF (CIPA DC-007) APP2 payload; returns (offset, size) of the
// second individual image, offset relative to the MP header (TIFF) start.
fn mpf_second_image(seg: &[u8]) -> Option<(usize, usize)> {
    if seg.len() < 14 || &seg[..4] != b"MPF\0" {
        return None;
    }
    let tiff = &seg[4..];
    let le = match &tiff[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16at = |o: usize| -> Option<u16> {
        if o + 2 <= tiff.len() {
            Some(if le { u16::from_le_bytes([tiff[o], tiff[o + 1]]) } else { u16::from_be_bytes([tiff[o], tiff[o + 1]]) })
        } else {
            None
        }
    };
    let u32at = |o: usize| -> Option<u32> {
        if o + 4 <= tiff.len() {
            Some(if le {
                u32::from_le_bytes([tiff[o], tiff[o + 1], tiff[o + 2], tiff[o + 3]])
            } else {
                u32::from_be_bytes([tiff[o], tiff[o + 1], tiff[o + 2], tiff[o + 3]])
            })
        } else {
            None
        }
    };
    if u16at(2)? != 42 {
        return None;
    }
    let ifd = u32at(4)? as usize;
    let count = u16at(ifd)? as usize;
    if count == 0 || count > 64 {
        return None;
    }
    let mut num_images = 0usize;
    let mut entries: Option<(usize, usize)> = None;
    for i in 0..count {
        let e = ifd + 2 + i * 12;
        let tag = u16at(e)?;
        let typ = u16at(e + 2)?;
        let cnt = u32at(e + 4)? as usize;
        match tag {
            0xB001 => num_images = if typ == 3 { u16at(e + 8)? as usize } else { u32at(e + 8)? as usize },
            0xB002 if (32..=1024).contains(&cnt) => {
                entries = Some((u32at(e + 8)? as usize, cnt));
            }
            _ => {}
        }
    }
    if num_images > 0 && num_images < 2 {
        return None;
    }
    let (mpe, _) = entries?;
    if mpe + 32 > tiff.len() {
        return None;
    }
    let size = u32at(mpe + 20)? as usize;
    let off = u32at(mpe + 24)? as usize;
    Some((off, size))
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
                // Apple-style gain map: base carries no HDR marker, the aux
                // image URN lives in the second MPF image (follows the photo)
                if marker == 0xE2 {
                    if let Some((img_off, img_size)) = mpf_second_image(seg) {
                        let abs = off + 8 + img_off;
                        if abs < raw.len() {
                            let end = if img_size == 0 { raw.len() } else { abs.saturating_add(img_size).min(raw.len()) };
                            let end = end.min(abs.saturating_add(4 * 1024 * 1024));
                            let second = &raw[abs..end];
                            if contains(second, b"hdrgainmap") || contains(second, b"HDRGainMap") {
                                return hdr("apple-gainmap");
                            }
                            if contains(second, b"hdrgm:")
                                || contains(second, b"hdr-gain-map")
                                || contains(second, b"urn:iso:std:iso:ts:21496:-1")
                            {
                                return hdr("jpeg-gainmap");
                            }
                        }
                    }
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

// ---- PNG high-fidelity compositing ----
// Decode at native bit depth, blend an RGBA watermark mask in the source's
// gamma domain (identical math to CSS/canvas alpha compositing), re-encode at
// the same bit depth, and pass through all ancillary chunks (iCCP/cICP/XMP/…)
// byte-for-byte.

const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

struct PngChunk {
    typ: [u8; 4],
    data: Vec<u8>,
}

fn parse_png_chunks(data: &[u8]) -> Result<Vec<PngChunk>, String> {
    if data.len() < 8 || data[..8] != PNG_SIG {
        return Err("not a PNG".to_string());
    }
    let mut chunks = Vec::new();
    let mut off = 8usize;
    loop {
        if off + 12 > data.len() {
            return Err("truncated PNG".to_string());
        }
        let len = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as usize;
        let typ = [data[off + 4], data[off + 5], data[off + 6], data[off + 7]];
        if off + 12 + len > data.len() {
            return Err("truncated PNG chunk".to_string());
        }
        chunks.push(PngChunk { typ, data: data[off + 8..off + 8 + len].to_vec() });
        off += 12 + len;
        if typ == *b"IEND" {
            break;
        }
    }
    Ok(chunks)
}

fn write_chunk(out: &mut Vec<u8>, typ: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(typ);
    out.extend_from_slice(data);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(typ);
    hasher.update(data);
    out.extend_from_slice(&hasher.finalize().to_be_bytes());
}

fn png_composite(
    original: &[u8],
    mask: &[u8],
    mask_w: usize,
    mask_h: usize,
    off_x: usize,
    off_y: usize,
) -> Result<Vec<u8>, String> {
    let chunks = parse_png_chunks(original)?;
    let ihdr = chunks.iter().find(|c| c.typ == *b"IHDR").ok_or("missing IHDR")?;
    if ihdr.data.len() < 13 {
        return Err("bad IHDR".to_string());
    }
    let color_type_raw = ihdr.data[9];
    if color_type_raw == 3 {
        return Err("palette PNG not supported".to_string());
    }
    if chunks.iter().any(|c| c.typ == *b"acTL") {
        return Err("APNG not supported".to_string());
    }

    // Decode at native bit depth (EXPAND: tRNS→alpha, <8bit gray→8bit)
    let mut decoder = png::Decoder::new(std::io::Cursor::new(original));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| format!("decode: {e}"))?;
    let mut buf = vec![0u8; reader.output_buffer_size().ok_or_else(|| "bad PNG geometry".to_string())?];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("decode: {e}"))?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width as usize, info.height as usize);
    let sixteen = info.bit_depth == png::BitDepth::Sixteen;
    let channels = info.color_type.samples() as usize;
    let bps = channels * if sixteen { 2 } else { 1 };

    if mask.len() < mask_w * mask_h * 4 {
        return Err("mask buffer too small".to_string());
    }
    if off_x + mask_w > w {
        return Err("mask wider than image".to_string());
    }
    // Vertical extension: watermark banner below the photo extends the canvas
    // (gap/pad rows filled with opaque white, matching the preview background)
    let out_h = h.max(off_y + mask_h);
    let row_bytes = w * bps;
    let mut canvas: Vec<u8> = if out_h > h {
        let mut ext = vec![0xFFu8; out_h * row_bytes];
        ext[..h * row_bytes].copy_from_slice(&buf);
        ext
    } else {
        buf
    };

    // Blend mask (RGBA8) into canvas, gamma-domain alpha compositing
    for my in 0..mask_h {
        for mx in 0..mask_w {
            let mi = (my * mask_w + mx) * 4;
            let a = mask[mi + 3] as u32;
            if a == 0 {
                continue;
            }
            let inv = 255 - a;
            let di = ((off_y + my) * w + (off_x + mx)) * bps;
            for c in 0..channels {
                // grayscale target uses mask luma; RGB(A) uses matching channel
                let wm8 = if channels >= 3 {
                    mask[mi + c.min(2)] as u32
                } else {
                    ((mask[mi] as u32 * 77 + mask[mi + 1] as u32 * 150 + mask[mi + 2] as u32 * 29) >> 8)
                };
                if sixteen {
                    let src = u16::from_ne_bytes([canvas[di + c * 2], canvas[di + c * 2 + 1]]) as u32;
                    let out = (src * inv + wm8 * 257 * a + 127) / 255;
                    let outb = (out as u16).to_ne_bytes();
                    canvas[di + c * 2] = outb[0];
                    canvas[di + c * 2 + 1] = outb[1];
                } else {
                    let src = canvas[di + c] as u32;
                    canvas[di + c] = ((src * inv + wm8 * a + 127) / 255) as u8;
                }
            }
        }
    }

    // Re-encode (height may be extended; color type/depth preserved)
    let mut encoded: Vec<u8> = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, info.width, out_h as u32);
        encoder.set_color(info.color_type);
        encoder.set_depth(info.bit_depth);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| format!("encode: {e}"))?;
        writer.write_image_data(&canvas).map_err(|e| format!("encode: {e}"))?;
    }
    let enc_chunks = parse_png_chunks(&encoded)?;

    // Assemble: sig + encoder IHDR (authoritative: geometry/interlace) + passthrough ancillary + IDAT* + IEND
    let enc_ihdr = enc_chunks.iter().find(|c| c.typ == *b"IHDR").ok_or("encoder produced no IHDR")?;
    let mut out = Vec::with_capacity(original.len() / 2 + encoded.len());
    out.extend_from_slice(&PNG_SIG);
    write_chunk(&mut out, b"IHDR", &enc_ihdr.data);
    // tRNS 仅在输出带 alpha 通道（ct 4/6）时丢弃，否则原样透传（色键对重编码像素仍有效）
    let has_alpha_out = enc_ihdr.data[9] == 4 || enc_ihdr.data[9] == 6;
    let skip: [&[u8; 4]; 7] = [b"IHDR", b"PLTE", b"IDAT", b"IEND", b"acTL", b"fcTL", b"fdAT"];
    for c in &chunks {
        if skip.iter().any(|s| **s == c.typ) {
            continue;
        }
        if c.typ == *b"tRNS" && has_alpha_out {
            continue;
        }
        write_chunk(&mut out, &c.typ, &c.data);
    }
    for c in enc_chunks.iter().filter(|c| c.typ == *b"IDAT") {
        write_chunk(&mut out, b"IDAT", &c.data);
    }
    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

#[wasm_bindgen]
pub fn composite_png(
    original: Vec<u8>,
    mask: Vec<u8>,
    mask_w: u32,
    mask_h: u32,
    off_x: u32,
    off_y: u32,
) -> Result<Vec<u8>, JsValue> {
    png_composite(
        &original,
        &mask,
        mask_w as usize,
        mask_h as usize,
        off_x as usize,
        off_y as usize,
    )
    .map_err(|e| JsValue::from_str(&e))
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

    // ---- PNG compositing tests ----

    fn encode_test_png(w: u32, h: u32, sixteen: bool, rgba: bool, fill: u16) -> Vec<u8> {
        let mut data = Vec::new();
        let channels = if rgba { 4u8 } else { 3u8 };
        for _ in 0..(w * h) {
            for c in 0..channels as u16 {
                if sixteen {
                    data.extend_from_slice(&(fill + c).to_ne_bytes());
                } else {
                    data.push((fill + c) as u8);
                }
            }
        }
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, w, h);
            encoder.set_color(if rgba { png::ColorType::Rgba } else { png::ColorType::Rgb });
            encoder.set_depth(if sixteen { png::BitDepth::Sixteen } else { png::BitDepth::Eight });
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&data).unwrap();
        }
        out
    }

    #[test]
    fn png16_endianness_roundtrip() {
        // 已知值 0x1234 编码后，经 crate 解码应还原（确认 native-endian 约定）
        let src = encode_test_png(2, 2, true, false, 0x1234);
        let decoder = png::Decoder::new(std::io::Cursor::new(&src));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        assert_eq!(info.bit_depth, png::BitDepth::Sixteen);
        let first = u16::from_ne_bytes([buf[0], buf[1]]);
        assert_eq!(first, 0x1234, "png crate 16bit samples must be native-endian");
    }

    #[test]
    fn png_composite_8bit_rgba() {
        let src = encode_test_png(8, 8, false, true, 100);
        // 4x4 白色 mask（alpha=255）放在 (2,2)
        let mask = vec![255u8; 4 * 4 * 4];
        let out = png_composite(&src, &mask, 4, 4, 2, 2).unwrap();
        // 重新解码验证
        let decoder = png::Decoder::new(std::io::Cursor::new(&out));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        assert_eq!(info.color_type, png::ColorType::Rgba);
        // mask 内 (3,3) → 白；mask 外 (0,0) → 原值 100
        let px = |x: usize, y: usize| (y * 8 + x) * 4;
        assert_eq!(&buf[px(3, 3)..px(3, 3) + 3], &[255, 255, 255]);
        assert_eq!(buf[px(0, 0)], 100);
        // 半透明混合：alpha=128 红 mask
        let mut mask2 = vec![0u8; 2 * 1 * 4];
        mask2[0..4].copy_from_slice(&[255, 0, 0, 128]);
        mask2[4..8].copy_from_slice(&[0, 0, 255, 64]);
        let out2 = png_composite(&src, &mask2, 2, 1, 0, 0).unwrap();
        let decoder2 = png::Decoder::new(std::io::Cursor::new(&out2));
        let mut reader2 = decoder2.read_info().unwrap();
        let mut buf2 = vec![0u8; reader2.output_buffer_size().unwrap()];
        reader2.next_frame(&mut buf2).unwrap();
        // (100*127 + 255*128 + 127)/255 = 178；G: (100*127+127)/255 = 50
        assert_eq!(buf2[0], 178, "alpha=128 blend red");
        assert_eq!(buf2[1], 50, "alpha=128 blend green");
    }

    #[test]
    fn png_composite_rejects_palette_and_apng() {
        let indexed = encode_test_png(4, 4, false, false, 10);
        // 手工把 IHDR colorType 改成 3 不现实（无 PLTE 无法解码），改用构造 acTL 测试 APNG 拒绝
        let mut apng = indexed.clone();
        // 在 IEND 前插入 acTL chunk
        let iend_pos = apng.len() - 12;
        let mut actl = Vec::new();
        write_chunk(&mut actl, b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0]);
        apng.splice(iend_pos..iend_pos, actl);
        let mask = vec![0u8; 4];
        assert!(png_composite(&apng, &mask, 1, 1, 0, 0).is_err());
        // 调色板：用 png crate 编码 indexed
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, 2, 2);
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_palette(vec![255, 0, 0, 0, 255, 0]);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0, 1, 1, 0]).unwrap();
        }
        assert!(png_composite(&out, &mask, 1, 1, 0, 0).is_err());
    }

    #[test]
    fn png_composite_geometry_expand() {
        // 4bit 灰度：EXPAND 到 8bit，输出 IHDR 必须取编码器侧
        let mut src4 = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut src4, 4, 4);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Four);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0]).unwrap();
        }
        let mask = vec![255u8; 2 * 2 * 4];
        let composited = png_composite(&src4, &mask, 2, 2, 0, 0).unwrap();
        let chunks = parse_png_chunks(&composited).unwrap();
        let ihdr = chunks.iter().find(|c| c.typ == *b"IHDR").unwrap();
        assert_eq!(ihdr.data[8], 8, "4bit gray must expand to 8bit in output IHDR");
        let decoder = png::Decoder::new(std::io::Cursor::new(&composited));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut buf).unwrap();

        // RGB + tRNS：png crate 不展开 tRNS，输出应保持 RGB 且 tRNS 原样透传
        let mut src_trns = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut src_trns, 2, 2);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_trns(vec![255, 0, 0]);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 1, 2, 3]).unwrap();
        }
        let mask2 = vec![0u8; 1 * 1 * 4];
        let out2 = png_composite(&src_trns, &mask2, 1, 1, 0, 0).unwrap();
        let chunks2 = parse_png_chunks(&out2).unwrap();
        let ihdr2 = chunks2.iter().find(|c| c.typ == *b"IHDR").unwrap();
        assert_eq!(ihdr2.data[9], 2, "RGB+tRNS must stay RGB (crate does not expand tRNS)");
        let src_trns_chunk = parse_png_chunks(&src_trns).unwrap().into_iter().find(|c| c.typ == *b"tRNS").unwrap();
        let out_trns_chunk = chunks2.iter().find(|c| c.typ == *b"tRNS").expect("tRNS must pass through");
        assert_eq!(out_trns_chunk.data, src_trns_chunk.data, "tRNS must be byte-identical");
        let decoder2 = png::Decoder::new(std::io::Cursor::new(&out2));
        let mut reader2 = decoder2.read_info().unwrap();
        let mut buf2 = vec![0u8; reader2.output_buffer_size().unwrap()];
        reader2.next_frame(&mut buf2).unwrap();
    }

    #[test]
    fn png_composite_extends_canvas_below() {
        // 8x8 图像，mask 放在 y=8（图像下方）→ 输出应为 8x12，扩展区白底 + mask 混合
        let src = encode_test_png(8, 8, false, true, 100);
        let (mw, mh) = (8usize, 4usize);
        let mut mask = vec![0u8; mw * mh * 4];
        // 第一行全透明白（模拟横幅文字），其余全透明
        for x in 0..mw {
            mask[x * 4..x * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
        let out = png_composite(&src, &mask, mw, mh, 0, 8).unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(&out));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        assert_eq!((info.width, info.height), (8, 12), "canvas must extend below");
        // y=8 行（mask 第一行）→ 白
        let row8 = 8 * 8 * 4;
        assert_eq!(&buf[row8..row8 + 3], &[255, 255, 255]);
        // y=9 行（mask 全透明区）→ 扩展白底
        let row9 = 9 * 8 * 4;
        assert_eq!(&buf[row9..row9 + 3], &[255, 255, 255]);
        // y=7 行（原图最后一行）→ 原值不变
        let row7 = 7 * 8 * 4;
        assert_eq!(buf[row7], 100);
    }

    #[test]
    fn png_composite_real_16bit_file() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test_pictures/IMG_9538.png");
        if !path.exists() {
            eprintln!("SKIP: {} not found (gitignored sample)", path.display());
            return;
        }
        let src = std::fs::read(&path).unwrap();
        let t0 = std::time::Instant::now();

        // 300x80 mask：左半不透明白，右半 alpha=128 纯红
        let (mw, mh) = (300usize, 80usize);
        let mut mask = vec![0u8; mw * mh * 4];
        for y in 0..mh {
            for x in 0..mw {
                let i = (y * mw + x) * 4;
                if x < mw / 2 {
                    mask[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                } else {
                    mask[i..i + 4].copy_from_slice(&[255, 0, 0, 128]);
                }
            }
        }
        let (ox, oy) = (100usize, 2900usize);
        let out = png_composite(&src, &mask, mw, mh, ox, oy).unwrap();
        let elapsed = t0.elapsed();
        eprintln!("composite time: {elapsed:?}, src={}B out={}B", src.len(), out.len());

        // 结构验证：chunk 直通
        let src_chunks = parse_png_chunks(&src).unwrap();
        let out_chunks = parse_png_chunks(&out).unwrap();
        for name in ["iCCP", "iTXt", "pHYs"] {
            let t: [u8; 4] = [name.as_bytes()[0], name.as_bytes()[1], name.as_bytes()[2], name.as_bytes()[3]];
            let s = src_chunks.iter().find(|c| c.typ == t).map(|c| &c.data);
            let o = out_chunks.iter().find(|c| c.typ == t).map(|c| &c.data);
            assert_eq!(s.is_some(), o.is_some(), "chunk {name} presence mismatch");
            if let (Some(a), Some(b)) = (s, o) {
                assert_eq!(a, b, "chunk {name} not byte-identical");
            }
        }
        let out_ihdr = out_chunks.iter().find(|c| c.typ == *b"IHDR").unwrap();
        assert_eq!(&out_ihdr.data[8], &16u8, "output must stay 16-bit");
        assert_eq!(out_ihdr.data[9], 2, "output must stay colorType RGB");

        // 像素验证：解码源与输出逐位对比
        let decode = |data: &[u8]| {
            let decoder = png::Decoder::new(std::io::Cursor::new(data));
            let mut reader = decoder.read_info().unwrap();
            let mut b = vec![0u8; reader.output_buffer_size().unwrap()];
            let inf = reader.next_frame(&mut b).unwrap();
            b.truncate(inf.buffer_size());
            (inf, b)
        };
        let (_, sb) = decode(&src);
        let (_, ob) = decode(&out);
        assert_eq!(sb.len(), ob.len());
        let (w, bps) = (4032usize, 6usize);
        let mut diff_outside = 0usize;
        let mut checked_inside = 0usize;
        let mut bad_inside = 0usize;
        for y in 0..3024usize {
            for x in 0..w {
                let in_mask = x >= ox && x < ox + mw && y >= oy && y < oy + mh;
                let di = (y * w + x) * bps;
                if !in_mask {
                    if sb[di..di + bps] != ob[di..di + bps] {
                        diff_outside += 1;
                    }
                } else if (x - ox) < mw / 2 {
                    // 不透明白 → 全 65535
                    checked_inside += 1;
                    for c in 0..3 {
                        let v = u16::from_ne_bytes([ob[di + c * 2], ob[di + c * 2 + 1]]);
                        if v != 65535 {
                            bad_inside += 1;
                            break;
                        }
                    }
                } else if y == oy {
                    // alpha=128 红：R = (src+65535*... )/255 公式验证一行
                    checked_inside += 1;
                    for c in 0..3 {
                        let s = u16::from_ne_bytes([sb[di + c * 2], sb[di + c * 2 + 1]]) as u32;
                        let wm = if c == 0 { 255u32 * 257 } else { 0 };
                        let expect = ((s * 127 + wm * 128 + 127) / 255) as u16;
                        let got = u16::from_ne_bytes([ob[di + c * 2], ob[di + c * 2 + 1]]);
                        if got != expect {
                            bad_inside += 1;
                            break;
                        }
                    }
                }
            }
        }
        assert_eq!(diff_outside, 0, "pixels outside mask must be bit-identical");
        assert!(checked_inside > 0);
        assert_eq!(bad_inside, 0, "blended pixels wrong");
    }

    // Build a JPEG whose APP2 carries an MPF directory pointing at an appended
    // second individual image (mirrors Apple gain map JPEG layout)
    fn mpf_test_jpeg(second: &[u8], num_images: u32, real_size: bool) -> Vec<u8> {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes());
        let mpe_off = 8 + 2 + 24 + 4;
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&0xB001u16.to_le_bytes());
        tiff.extend_from_slice(&4u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&num_images.to_le_bytes());
        tiff.extend_from_slice(&0xB002u16.to_le_bytes());
        tiff.extend_from_slice(&7u16.to_le_bytes());
        tiff.extend_from_slice(&32u32.to_le_bytes());
        tiff.extend_from_slice(&(mpe_off as u32).to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&4u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&(if real_size { second.len() as u32 } else { 0 }).to_le_bytes());
        let off_field = tiff.len();
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());

        let mut jpeg = vec![0xFF, 0xD8];
        let payload_len = 4 + tiff.len();
        jpeg.extend_from_slice(&[0xFF, 0xE2]);
        jpeg.extend_from_slice(&((payload_len + 2) as u16).to_be_bytes());
        jpeg.extend_from_slice(b"MPF\0");
        let tiff_start = jpeg.len();
        jpeg.extend_from_slice(&tiff);
        jpeg.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        let img_abs = jpeg.len();
        let off_val = (img_abs - tiff_start) as u32;
        jpeg[tiff_start + off_field..tiff_start + off_field + 4].copy_from_slice(&off_val.to_le_bytes());
        jpeg.extend_from_slice(second);
        jpeg
    }

    fn fake_aux_image(meta: &[u8]) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE1];
        let payload_len = 4 + meta.len();
        v.extend_from_slice(&((payload_len + 2) as u16).to_be_bytes());
        v.extend_from_slice(b"XMP\0");
        v.extend_from_slice(meta);
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    #[test]
    fn jpeg_mpf_apple_gainmap_is_hdr() {
        let meta = b"<x:xmpmeta xmlns:apdi='http://ns.apple.com/HDRGainMap/1.0/'><apdi:AuxiliaryImageType>urn:com:apple:photo:2020:aux:hdrgainmap</apdi:AuxiliaryImageType></x:xmpmeta>";
        let jpeg = mpf_test_jpeg(&fake_aux_image(meta), 2, true);
        let info = detect_hdr_inner(&jpeg);
        assert!(info.is_hdr && info.kind == "apple-gainmap");
    }

    #[test]
    fn jpeg_mpf_iso_or_xmp_gainmap_is_hdr() {
        for meta in [b"urn:iso:std:iso:ts:21496:-1".as_slice(), b"hdrgm:Version='1.0'".as_slice()] {
            let jpeg = mpf_test_jpeg(&fake_aux_image(meta), 2, true);
            let info = detect_hdr_inner(&jpeg);
            assert!(info.is_hdr && info.kind == "jpeg-gainmap");
        }
    }

    #[test]
    fn jpeg_mpf_stereo_pair_is_not_hdr() {
        let jpeg = mpf_test_jpeg(&fake_aux_image(b"plain second image, no gain map metadata"), 2, true);
        assert!(!detect_hdr_inner(&jpeg).is_hdr);
    }

    #[test]
    fn jpeg_mpf_single_image_is_not_hdr() {
        let meta = b"urn:com:apple:photo:2020:aux:hdrgainmap";
        let jpeg = mpf_test_jpeg(&fake_aux_image(meta), 1, true);
        assert!(!detect_hdr_inner(&jpeg).is_hdr);
    }

    #[test]
    fn jpeg_mpf_zero_size_scans_tail() {
        let meta = b"urn:com:apple:photo:2020:aux:hdrgainmap";
        let jpeg = mpf_test_jpeg(&fake_aux_image(meta), 2, false);
        let info = detect_hdr_inner(&jpeg);
        assert!(info.is_hdr && info.kind == "apple-gainmap");
    }

    #[test]
    fn jpeg_mpf_truncation_is_safe() {
        let meta = b"urn:com:apple:photo:2020:aux:hdrgainmap";
        let jpeg = mpf_test_jpeg(&fake_aux_image(meta), 2, true);
        let full = jpeg.len();
        for cut in [8usize, 20, 40, full / 2, full - 1] {
            let _ = detect_hdr_inner(&jpeg[..cut.min(full)]);
        }
        for shrink in [8usize, 9, 10, 12] {
            let _ = detect_hdr_inner(&jpeg[..full - shrink]);
        }
    }

    #[test]
    fn real_hdr_samples_if_present() {
        let cases = [
            ("../test_pictures/IMG_9519.HEIC", "apple-gainmap"),
            ("../test_pictures/IMG_9538.HEIC", "iso-21496-1"),
            ("../test_pictures/apple_gainmap_new.jpg", "apple-gainmap"),
            ("../test_pictures/apple_gainmap_old.jpg", "apple-gainmap"),
            ("../test_pictures/synthetic-ultrahdr.jpg", "jpeg-gainmap"),
        ];
        let mut ran = 0;
        for (p, kind) in cases {
            if let Ok(b) = std::fs::read(p) {
                ran += 1;
                let info = detect_hdr_inner(&b);
                assert!(info.is_hdr, "{p} must be HDR");
                assert_eq!(info.kind, kind, "{p} kind");
            }
        }
        if ran == 0 {
            eprintln!("SKIP: no local samples in test_pictures/");
        }
    }
}
