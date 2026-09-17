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

fn find_sub(data: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > data.len() {
        return None;
    }
    data.windows(needle.len()).position(|w| w == needle)
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
            if typ == b"cICP" && len >= 2 && body[1] == 16 {
                return hdr("png-pq");
            }
            if typ == b"cICP" && len >= 2 && body[1] == 18 {
                return hdr("png-hlg");
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

// ---- Ultra HDR (MPF gain map JPEG) assembly ----
// Keep the watermarked SDR base, re-attach the original gain map with its
// metadata (XMP hdrgm / ISO 21496-1 APP2 / ICC), and rewrite the MPF directory.
// Layout follows libultrahdr v1.4 output (verified against ultrahdr_app).

const ISO21496_URN: &[u8] = b"urn:iso:std:iso:ts:21496:-1\0";
const XMP_SIG: &[u8] = b"http://ns.adobe.com/xap/1.0/";
const MPF_MARKER_LEN: usize = 90;

struct JpegSegment<'a> {
    marker: u8,
    payload: &'a [u8],
    start: usize,
}

fn jpeg_segments(raw: &[u8]) -> Vec<JpegSegment<'_>> {
    let mut out = Vec::new();
    if raw.len() < 4 || raw[0] != 0xFF || raw[1] != 0xD8 {
        return out;
    }
    let mut off = 2usize;
    while off + 4 <= raw.len() {
        if raw[off] != 0xFF {
            break;
        }
        let marker = raw[off + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            off += 2;
            continue;
        }
        let seg_len = u16::from_be_bytes([raw[off + 2], raw[off + 3]]) as usize;
        if seg_len < 2 || off + 2 + seg_len > raw.len() {
            break;
        }
        out.push(JpegSegment { marker, payload: &raw[off + 4..off + 2 + seg_len], start: off });
        off += 2 + seg_len;
    }
    out
}

fn jpeg_app_insert_pos(raw: &[u8]) -> usize {
    let mut off = 2usize;
    while off + 4 <= raw.len() {
        if raw[off] != 0xFF {
            break;
        }
        let marker = raw[off + 1];
        if !((0xE0..=0xEF).contains(&marker) || marker == 0xFE) {
            break;
        }
        let seg_len = u16::from_be_bytes([raw[off + 2], raw[off + 3]]) as usize;
        if seg_len < 2 || off + 2 + seg_len > raw.len() {
            break;
        }
        off += 2 + seg_len;
    }
    off
}

fn write_jpeg_segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    if payload.len() + 2 > u16::MAX as usize {
        return;
    }
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(payload);
}

fn has_segment(raw: &[u8], marker: u8, payload: &[u8]) -> bool {
    jpeg_segments(raw).iter().any(|s| s.marker == marker && s.payload == payload)
}

// The Google container directory (XMP) declares the embedded gain map item
// with Item:Length. The packet is carried verbatim from the source, so after a
// re-encode the declared value would describe the original bytes. Rewrite it to
// the gain map that is actually attached.
fn patch_container_gainmap_length(xmp: &mut Vec<u8>, new_len: usize) {
    let Some(sem) = find_sub(xmp, b"Semantic=\"GainMap\"") else { return };
    let Some(rel) = find_sub(&xmp[sem..], b"Length=\"") else { return };
    let start = sem + rel + b"Length=\"".len();
    let Some(quote) = xmp[start..].iter().position(|&b| b == b'"') else { return };
    let end = start + quote;
    let new = new_len.to_string();
    if xmp[start..end] != *new.as_bytes() {
        xmp.splice(start..end, new.into_bytes());
    }
}

fn patch_xmp_in_jpeg(jpeg: &mut Vec<u8>, new_len: usize) {
    let mut off = 2usize;
    while off + 4 <= jpeg.len() {
        if jpeg[off] != 0xFF {
            break;
        }
        let m = jpeg[off + 1];
        if m == 0xDA || m == 0xD9 {
            break;
        }
        if m == 0x01 || (0xD0..=0xD7).contains(&m) {
            off += 2;
            continue;
        }
        let len = u16::from_be_bytes([jpeg[off + 2], jpeg[off + 3]]) as usize;
        if len < 2 || off + 2 + len > jpeg.len() {
            break;
        }
        let start = off + 4;
        let end = off + 2 + len;
        if jpeg[start..].starts_with(XMP_SIG) {
            let mut payload = jpeg[start..end].to_vec();
            patch_container_gainmap_length(&mut payload, new_len);
            let delta = payload.len() as isize - (end - start) as isize;
            jpeg.splice(start..end, payload);
            if delta != 0 {
                let new_field = (len as isize + delta) as u16;
                jpeg[off + 2..off + 4].copy_from_slice(&new_field.to_be_bytes());
            }
            return;
        }
        off = end;
    }
}

// HDR metadata segments: XMP carrying hdrgm, ISO 21496-1 APP2, optionally ICC
fn hdr_carry_segments(raw: &[u8], include_icc: bool) -> Vec<(u8, Vec<u8>)> {
    let mut out: Vec<(u8, Vec<u8>)> = Vec::new();
    for s in jpeg_segments(raw) {
        let is_hdr_meta = (s.marker == 0xE1 && s.payload.starts_with(XMP_SIG)
            && (contains(s.payload, b"hdrgm:") || contains(s.payload, b"hdr-gain-map")))
            || (s.marker == 0xE2 && s.payload.starts_with(ISO21496_URN))
            || (include_icc && s.marker == 0xE2 && s.payload.starts_with(b"ICC_PROFILE\0"));
        if !is_hdr_meta {
            continue;
        }
        if s.payload.len() + 2 > u16::MAX as usize {
            continue;
        }
        if !out.iter().any(|(m, p)| *m == s.marker && p.as_slice() == s.payload) {
            out.push((s.marker, s.payload.to_vec()));
        }
    }
    out
}

fn strip_app2(raw: &[u8], prefix: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    if raw.len() < 2 {
        return raw.to_vec();
    }
    out.extend_from_slice(&raw[..2]);
    let mut off = 2usize;
    while off + 4 <= raw.len() {
        if raw[off] != 0xFF {
            break;
        }
        let marker = raw[off + 1];
        if marker == 0xDA || marker == 0xD9 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            break;
        }
        let seg_len = u16::from_be_bytes([raw[off + 2], raw[off + 3]]) as usize;
        if seg_len < 2 || off + 2 + seg_len > raw.len() {
            break;
        }
        let payload = &raw[off + 4..off + 2 + seg_len];
        let drop = marker == 0xE2 && payload.starts_with(prefix);
        if !drop {
            out.extend_from_slice(&raw[off..off + 2 + seg_len]);
        }
        off += 2 + seg_len;
    }
    out.extend_from_slice(&raw[off..]);
    out
}

fn strip_mpf(raw: &[u8]) -> Vec<u8> {
    strip_app2(raw, b"MPF\0")
}

fn strip_icc(raw: &[u8]) -> Vec<u8> {
    strip_app2(raw, b"ICC_PROFILE\0")
}

fn gainmap_range(raw: &[u8]) -> Option<(usize, usize)> {
    for s in jpeg_segments(raw) {
        if s.marker != 0xE2 {
            continue;
        }
        if let Some((img_off, img_size)) = mpf_second_image(s.payload) {
            let abs = s.start + 8 + img_off;
            if abs + 4 <= raw.len() && raw[abs] == 0xFF && raw[abs + 1] == 0xD8 {
                let size = if img_size == 0 { raw.len() - abs } else { img_size.min(raw.len() - abs) };
                return Some((abs, size));
            }
        }
    }
    // MPF offsets can be stale (editors rewrite the metadata segments or
    // re-encode the base without patching them). Fall back to the second
    // concatenated JPEG, which is how libultrahdr's decoder and Apple locate
    // the gain map.
    sequential_gainmap_range(raw)
}

// Walk a JPEG from `start` through its EOI; returns the index just past EOI.
// Segment payloads are skipped by length, so embedded thumbnails are ignored;
// in entropy data only stuffed 0xFF00, RSTn markers and EOI appear, and
// progressive scans (extra SOS) are walked like any other segment.
fn jpeg_image_end(raw: &[u8], start: usize) -> Option<usize> {
    if start + 2 > raw.len() || raw[start] != 0xFF || raw[start + 1] != 0xD8 {
        return None;
    }
    let mut off = start + 2;
    while off + 1 < raw.len() {
        if raw[off] != 0xFF {
            off += 1;
            continue;
        }
        let m = raw[off + 1];
        if m == 0x00 || (0xD0..=0xD7).contains(&m) {
            off += 2;
            continue;
        }
        if m == 0xD9 {
            return Some(off + 2);
        }
        if m == 0x01 {
            off += 2;
            continue;
        }
        if off + 4 > raw.len() {
            return None;
        }
        let len = u16::from_be_bytes([raw[off + 2], raw[off + 3]]) as usize;
        if len < 2 || off + 2 + len > raw.len() {
            return None;
        }
        off += 2 + len;
    }
    None
}

fn sequential_gainmap_range(raw: &[u8]) -> Option<(usize, usize)> {
    let primary_end = jpeg_image_end(raw, 0)?;
    let mut p = primary_end;
    while p + 3 < raw.len() {
        if raw[p] == 0xFF && raw[p + 1] == 0xD8 && raw[p + 2] == 0xFF && raw[p + 3] != 0x00 {
            let end = jpeg_image_end(raw, p).unwrap_or(raw.len());
            return Some((p, end - p));
        }
        p += 1;
    }
    None
}

fn ultrahdr_gainmap_inner(raw: &[u8]) -> Option<Vec<u8>> {
    let (abs, size) = gainmap_range(raw)?;
    Some(raw[abs..abs + size].to_vec())
}

// ISO 21496-1 APP2 body after the URN: version(4) + flags + per-channel
// min/max/gamma/offsets as fractions (libultrahdr gainmapmetadata.cpp layout)
fn iso_neutral_values(payload: &[u8]) -> Option<(bool, [u8; 3])> {
    let data = payload.strip_prefix(ISO21496_URN)?;
    if data.len() < 5 || u16::from_be_bytes([data[0], data[1]]) != 0 {
        return None;
    }
    let flags = data[4];
    if flags & 4 != 0 {
        return None; // backward direction (HDR as base) is not supported
    }
    let multi = flags & 0x80 != 0;
    let common = flags & 8 != 0;
    let ch_count = if multi { 3usize } else { 1usize };
    let mut pos = 5usize;
    let u32at = |pos: &mut usize| -> Option<u32> {
        let v = u32::from_be_bytes([*data.get(*pos)?, *data.get(*pos + 1)?, *data.get(*pos + 2)?, *data.get(*pos + 3)?]);
        *pos += 4;
        Some(v)
    };
    let s32at = |pos: &mut usize| -> Option<i32> {
        let v = i32::from_be_bytes([*data.get(*pos)?, *data.get(*pos + 1)?, *data.get(*pos + 2)?, *data.get(*pos + 3)?]);
        *pos += 4;
        Some(v)
    };
    let mut channels: Vec<(f64, f64, f64)> = Vec::with_capacity(ch_count);
    if common {
        let den = u32at(&mut pos)?;
        u32at(&mut pos)?;
        u32at(&mut pos)?;
        if den == 0 {
            return None;
        }
        for _ in 0..ch_count {
            let mn = s32at(&mut pos)? as f64 / den as f64;
            let mx = s32at(&mut pos)? as f64 / den as f64;
            let ga = u32at(&mut pos)? as f64 / den as f64;
            s32at(&mut pos)?;
            s32at(&mut pos)?;
            channels.push((mn, mx, ga));
        }
    } else {
        let _bh_n = u32at(&mut pos)?;
        let bh_d = u32at(&mut pos)?;
        let _ah_n = u32at(&mut pos)?;
        let ah_d = u32at(&mut pos)?;
        if bh_d == 0 || ah_d == 0 {
            return None;
        }
        for _ in 0..ch_count {
            let min_n = s32at(&mut pos)?;
            let min_d = u32at(&mut pos)?;
            let max_n = s32at(&mut pos)?;
            let max_d = u32at(&mut pos)?;
            let gamma_n = u32at(&mut pos)?;
            let gamma_d = u32at(&mut pos)?;
            let _ = (s32at(&mut pos)?, u32at(&mut pos)?);
            let _ = (s32at(&mut pos)?, u32at(&mut pos)?);
            if min_d == 0 || max_d == 0 || gamma_d == 0 {
                return None;
            }
            channels.push((min_n as f64 / min_d as f64, max_n as f64 / max_d as f64, gamma_n as f64 / gamma_d as f64));
        }
    }
    let mut values = [0u8; 3];
    for (i, (mn, mx, ga)) in channels.iter().enumerate() {
        values[if multi { i } else { 0 }] = neutral_value(*mn, *mx, *ga)?;
    }
    if !multi {
        values[1] = values[0];
        values[2] = values[0];
    }
    Some((multi, values))
}

// Minimal parse of hdrgm XMP attributes (single value or comma list)
fn xmp_attr_floats(payload: &[u8], name: &str) -> Vec<f64> {
    let needle = [b"hdrgm:", name.as_bytes()].concat();
    let mut at = 0usize;
    while let Some(i) = payload[at..].windows(needle.len()).position(|w| w == needle.as_slice()).map(|p| p + at) {
        let mut j = i + needle.len();
        while j < payload.len() && payload[j] == b' ' {
            j += 1;
        }
        if j < payload.len() && payload[j] == b'=' {
            j += 1;
            while j < payload.len() && payload[j] == b' ' {
                j += 1;
            }
            if j < payload.len() && (payload[j] == b'"' || payload[j] == b'\'') {
                let quote = payload[j];
                let start = j + 1;
                let mut end = start;
                while end < payload.len() && payload[end] != quote {
                    end += 1;
                }
                let text = String::from_utf8_lossy(&payload[start..end]);
                let vals: Vec<f64> = text
                    .split([',', ' '])
                    .filter(|t| !t.is_empty())
                    .filter_map(|t| t.parse::<f64>().ok())
                    .collect();
                if !vals.is_empty() {
                    return vals;
                }
            }
        }
        at = i + needle.len();
    }
    Vec::new()
}

// Encoded sample that reconstructs gain = 1 (log2 boost 0): the neutrality
// point of the gain map; math from libultrahdr affineMapGain/applyGain
fn neutral_value(min_log2: f64, max_log2: f64, gamma: f64) -> Option<u8> {
    if !min_log2.is_finite() || !max_log2.is_finite() || !gamma.is_finite() || gamma <= 0.0 {
        return None;
    }
    let span = max_log2 - min_log2;
    if span.abs() < 1e-9 {
        return None;
    }
    let g = -min_log2 / span;
    if !(0.0..=1.0).contains(&g) {
        return None;
    }
    let sample = g.powf(gamma);
    let code = (sample * 255.0 + 0.5).floor().clamp(0.0, 255.0) as u8;
    Some(code)
}

fn xmp_neutral_values(payload: &[u8]) -> Option<(bool, [u8; 3])> {
    let maxs = xmp_attr_floats(payload, "GainMapMax");
    if maxs.is_empty() {
        return None;
    }
    let mins = xmp_attr_floats(payload, "GainMapMin");
    let gammas = xmp_attr_floats(payload, "Gamma");
    let count = maxs.len().min(3);
    let mut values = [0u8; 3];
    for i in 0..count {
        let mn = mins.get(i).copied().unwrap_or_else(|| mins.first().copied().unwrap_or(0.0));
        let mx = maxs[i];
        let ga = gammas.get(i).copied().unwrap_or_else(|| gammas.first().copied().unwrap_or(1.0));
        values[i] = neutral_value(mn, mx, ga)?;
    }
    if count == 1 {
        values[1] = values[0];
        values[2] = values[0];
    }
    Some((count > 1, values))
}

#[derive(Serialize)]
struct NeutralInfo {
    ok: bool,
    multi_channel: bool,
    values: [u8; 3],
}

fn ultrahdr_neutral_inner(raw: &[u8]) -> NeutralInfo {
    let mut iso_payloads: Vec<&[u8]> = Vec::new();
    let mut xmp_payloads: Vec<&[u8]> = Vec::new();
    for s in jpeg_segments(raw) {
        if s.marker == 0xE2 && s.payload.starts_with(ISO21496_URN) && s.payload.len() > 32 {
            iso_payloads.push(s.payload);
        }
        if s.marker == 0xE1 && s.payload.starts_with(XMP_SIG) && contains(s.payload, b"hdrgm:") {
            xmp_payloads.push(s.payload);
        }
    }
    if let Some((abs, size)) = gainmap_range(raw) {
        let gm = &raw[abs..abs + size];
        for s in jpeg_segments(gm) {
            if s.marker == 0xE2 && s.payload.starts_with(ISO21496_URN) && s.payload.len() > 32 {
                iso_payloads.push(s.payload);
            }
            if s.marker == 0xE1 && s.payload.starts_with(XMP_SIG) && contains(s.payload, b"hdrgm:") {
                xmp_payloads.push(s.payload);
            }
        }
    }
    for payload in iso_payloads {
        if let Some((multi, values)) = iso_neutral_values(payload) {
            return NeutralInfo { ok: true, multi_channel: multi, values };
        }
    }
    for payload in xmp_payloads {
        if let Some((multi, values)) = xmp_neutral_values(payload) {
            return NeutralInfo { ok: true, multi_channel: multi, values };
        }
    }
    NeutralInfo { ok: false, multi_channel: false, values: [0; 3] }
}

fn build_mpf_segment(base_total_len: usize, gm_len: usize, gm_offset: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(MPF_MARKER_LEN);
    out.extend_from_slice(&[0xFF, 0xE2, 0x00, 0x58]);
    out.extend_from_slice(b"MPF\0");
    out.extend_from_slice(b"MM\x00\x2A\x00\x00\x00\x08");
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&0xB000u16.to_be_bytes());
    out.extend_from_slice(&7u16.to_be_bytes());
    out.extend_from_slice(&4u32.to_be_bytes());
    out.extend_from_slice(b"0100");
    out.extend_from_slice(&0xB001u16.to_be_bytes());
    out.extend_from_slice(&4u16.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&0xB002u16.to_be_bytes());
    out.extend_from_slice(&7u16.to_be_bytes());
    out.extend_from_slice(&32u32.to_be_bytes());
    out.extend_from_slice(&50u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0x00030000u32.to_be_bytes());
    out.extend_from_slice(&(base_total_len as u32).to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(gm_len as u32).to_be_bytes());
    out.extend_from_slice(&(gm_offset as u32).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out
}

fn insert_segments(raw: &mut Vec<u8>, at: usize, segs: &[(u8, Vec<u8>)]) {
    if segs.is_empty() {
        return;
    }
    let mut buf = Vec::new();
    for (marker, payload) in segs {
        write_jpeg_segment(&mut buf, *marker, payload);
    }
    raw.splice(at..at, buf);
}

fn ultrahdr_assemble_inner(original: &[u8], base: &[u8], gainmap: &[u8]) -> Result<Vec<u8>, String> {
    if base.len() < 4 || base[0] != 0xFF || base[1] != 0xD8 {
        return Err("base is not a JPEG".to_string());
    }
    if gainmap.len() < 4 || gainmap[0] != 0xFF || gainmap[1] != 0xD8 {
        return Err("gain map is not a JPEG".to_string());
    }
    let (orig_gm_abs, orig_gm_size) = gainmap_range(original).ok_or("original has no MPF gain map")?;
    let orig_gm = &original[orig_gm_abs..orig_gm_abs + orig_gm_size];

    // Gain map carries its own metadata (ISO payload / ICC); re-attach it when
    // the caller supplied a re-encoded gain map image. A re-encoded gain map may
    // carry an encoder-added ICC (canvas sRGB) — replace it with the original
    // so exactly one profile (the HDR intent one) survives.
    let carry = hdr_carry_segments(orig_gm, true);
    let orig_icc = carry
        .iter()
        .find(|(m, p)| *m == 0xE2 && p.starts_with(b"ICC_PROFILE\0"))
        .map(|(_, p)| p.clone());
    let mut gm = gainmap.to_vec();
    if let Some(icc) = &orig_icc {
        let has_any_icc = jpeg_segments(&gm).iter().any(|s| s.marker == 0xE2 && s.payload.starts_with(b"ICC_PROFILE\0"));
        if has_any_icc && !has_segment(&gm, 0xE2, icc) {
            gm = strip_icc(&gm);
        }
    }
    let gm_ins: Vec<(u8, Vec<u8>)> = carry
        .into_iter()
        .filter(|(m, p)| !has_segment(&gm, *m, p))
        .collect();
    let gm_at = jpeg_app_insert_pos(&gm);
    insert_segments(&mut gm, gm_at, &gm_ins);

    // Base carries the XMP / ISO marker; strip any stale MPF before re-adding
    let mut out = strip_mpf(base);
    let base_ins: Vec<(u8, Vec<u8>)> = hdr_carry_segments(original, false)
        .into_iter()
        .filter(|(m, p)| !has_segment(&out, *m, p))
        .collect();
    let base_at = jpeg_app_insert_pos(&out);
    insert_segments(&mut out, base_at, &base_ins);
    patch_xmp_in_jpeg(&mut out, gm.len());

    let mpf_at = jpeg_app_insert_pos(&out);
    let base_total_len = out.len() + MPF_MARKER_LEN;
    let gm_offset = base_total_len - (mpf_at + 8);
    let mpf = build_mpf_segment(base_total_len, gm.len(), gm_offset);
    out.splice(mpf_at..mpf_at, mpf);
    out.extend_from_slice(&gm);
    Ok(out)
}

#[wasm_bindgen]
pub fn ultrahdr_gainmap(raw: Vec<u8>) -> Result<Vec<u8>, JsValue> {
    ultrahdr_gainmap_inner(&raw).ok_or_else(|| JsValue::from_str("no MPF gain map found"))
}

#[wasm_bindgen]
pub fn ultrahdr_neutral(raw: Vec<u8>) -> JsValue {
    let info = ultrahdr_neutral_inner(&raw);
    <wasm_bindgen::JsValue as JsValueSerdeExt>::from_serde(&info).unwrap()
}

#[wasm_bindgen]
pub fn ultrahdr_assemble(original: Vec<u8>, base: Vec<u8>, gainmap: Vec<u8>) -> Result<Vec<u8>, JsValue> {
    ultrahdr_assemble_inner(&original, &base, &gainmap).map_err(|e| JsValue::from_str(&e))
}

// ---- Ultra HDR creation from scratch (Apple HDR HEIC -> gain map JPEG) ----
// Writes the ISO 21496-1 APP2 payload (libultrahdr `encodeGainmapMetadata`
// layout, common denominator form, single channel, gamma 1) plus the hdrgm
// XMP, then assembles base + gain map with a freshly computed MPF directory.

const ISO21496_DENOM: u32 = 1_000_000;

// boost factor is `2^(log2(headroom) * v)` for the normalized sample v, so the
// Apple gain map value maps to v = log2(1 + (headroom-1) * srgb_eotf(g)) / log2(headroom)
fn build_iso21496_1_payload(headroom: f64) -> Result<Vec<u8>, String> {
    if !headroom.is_finite() || headroom <= 1.0 {
        return Err("invalid headroom".to_string());
    }
    let gain_max = headroom.log2();
    let den = ISO21496_DENOM;
    let mut out = Vec::with_capacity(ISO21496_URN.len() + 4 + 1 + 4 * 8);
    out.extend_from_slice(ISO21496_URN);
    out.extend_from_slice(&0u16.to_be_bytes()); // min_version
    out.extend_from_slice(&0u16.to_be_bytes()); // writer_version
    out.push(0x08); // common denominator, single channel, forward direction
    out.extend_from_slice(&den.to_be_bytes());
    out.extend_from_slice(&den.to_be_bytes()); // baseHdrHeadroom (log2 0 = no boost)
    out.extend_from_slice(&((gain_max * den as f64).round() as u32).to_be_bytes()); // alternateHdrHeadroom
    out.extend_from_slice(&0i32.to_be_bytes()); // gainMapMin
    out.extend_from_slice(&((gain_max * den as f64).round() as i32).to_be_bytes()); // gainMapMax
    out.extend_from_slice(&den.to_be_bytes()); // gainMapGamma = 1.0
    out.extend_from_slice(&0i32.to_be_bytes()); // baseOffset
    out.extend_from_slice(&0i32.to_be_bytes()); // alternateOffset
    Ok(out)
}

fn build_secondary_xmp(headroom: f64) -> Vec<u8> {
    let gain_max = headroom.log2();
    let xml = format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"XMP Core 5.5.0\">\n \
<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  \
<rdf:Description rdf:about=\"\"\n    \
xmlns:hdrgm=\"http://ns.adobe.com/hdr-gain-map/1.0/\"\n   \
hdrgm:Version=\"1.0\"\n   \
hdrgm:GainMapMin=\"0.000000\"\n   \
hdrgm:GainMapMax=\"{gain_max:.6}\"\n   \
hdrgm:HDRCapacityMin=\"0.000000\"\n   \
hdrgm:HDRCapacityMax=\"{gain_max:.6}\"\n   \
hdrgm:OffsetSDR=\"0.000000\"\n   \
hdrgm:OffsetHDR=\"0.000000\"/>\n \
</rdf:RDF>\n\
</x:xmpmeta>\n       \n\
<?xpacket end=\"w\"?>"
    );
    let mut out = Vec::with_capacity(XMP_SIG.len() + 1 + xml.len());
    out.extend_from_slice(XMP_SIG);
    out.push(0);
    out.extend_from_slice(xml.as_bytes());
    out
}

// Drops hdrgm XMP / ISO 21496-1 / MPF segments so freshly generated metadata
// cannot collide with a stale copy carried by the input.
fn strip_hdr_metadata(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    if raw.len() < 2 {
        return raw.to_vec();
    }
    out.extend_from_slice(&raw[..2]);
    let mut off = 2usize;
    while off + 4 <= raw.len() {
        if raw[off] != 0xFF {
            break;
        }
        let marker = raw[off + 1];
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            off += 2;
            continue;
        }
        let len = u16::from_be_bytes([raw[off + 2], raw[off + 3]]) as usize;
        if len < 2 || off + 2 + len > raw.len() {
            break;
        }
        let payload = &raw[off + 4..off + 2 + len];
        let drop = (marker == 0xE1 && payload.starts_with(XMP_SIG) && contains(payload, b"hdrgm:"))
            || (marker == 0xE2 && (payload.starts_with(ISO21496_URN) || payload.starts_with(b"MPF\0")));
        if !drop {
            out.extend_from_slice(&raw[off..off + 2 + len]);
        }
        off += 2 + len;
    }
    out.extend_from_slice(&raw[off..]);
    out
}

fn build_primary_xmp(gainmap_len: usize) -> Vec<u8> {
    let xml = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\" x:xmptk=\"Adobe XMP Core 5.1.2\">\n  \
<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n    \
<rdf:Description\n        \
xmlns:Container=\"http://ns.google.com/photos/1.0/container/\"\n        \
xmlns:Item=\"http://ns.google.com/photos/1.0/container/item/\"\n        \
xmlns:hdrgm=\"http://ns.adobe.com/hdr-gain-map/1.0/\"\n        \
hdrgm:Version=\"1.0\">\n      \
<Container:Directory>\n        \
<rdf:Seq>\n          \
<rdf:li rdf:parseType=\"Resource\">\n            \
<Container:Item\n             Item:Semantic=\"Primary\"\n             Item:Mime=\"image/jpeg\"/>\n          \
</rdf:li>\n          \
<rdf:li rdf:parseType=\"Resource\">\n            \
<Container:Item\n             Item:Semantic=\"GainMap\"\n             Item:Mime=\"image/jpeg\"\n             Item:Length=\"{gainmap_len}\"/>\n          \
</rdf:li>\n        \
</rdf:Seq>\n      \
</Container:Directory>\n    \
</rdf:Description>\n  \
</rdf:RDF>\n\
</x:xmpmeta>"
    );
    let mut out = Vec::with_capacity(XMP_SIG.len() + 1 + xml.len());
    out.extend_from_slice(XMP_SIG);
    out.push(0);
    out.extend_from_slice(xml.as_bytes());
    out
}

fn apple_hdr_iso_gainmap_inner(gainmap: &[u8], gm_w: usize, gm_h: usize, headroom: f64) -> Result<Vec<u8>, String> {
    if gm_w == 0 || gm_h == 0 || gainmap.len() < gm_w * gm_h * 4 {
        return Err("bad gain map".to_string());
    }
    if !headroom.is_finite() || headroom <= 1.0 {
        return Err("invalid headroom".to_string());
    }
    let srgb = SrgbEotf::new();
    let denom = headroom.log2();
    let scale_range = headroom - 1.0;
    let mut out = vec![0u8; gm_w * gm_h];
    for (i, o) in out.iter_mut().enumerate() {
        let g = gainmap[i * 4] as f64 / 255.0;
        let ratio = 1.0 + scale_range * srgb.eval(g);
        let v = ratio.log2() / denom;
        *o = (v.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8;
    }
    Ok(out)
}

fn ultrahdr_create_inner(base: &[u8], gainmap: &[u8], headroom: f64) -> Result<Vec<u8>, String> {
    if base.len() < 4 || base[0] != 0xFF || base[1] != 0xD8 {
        return Err("base is not a JPEG".to_string());
    }
    if gainmap.len() < 4 || gainmap[0] != 0xFF || gainmap[1] != 0xD8 {
        return Err("gain map is not a JPEG".to_string());
    }
    let iso = build_iso21496_1_payload(headroom)?;

    // gain map image carries the full hdrgm metadata (XMP + ISO APP2); drop any
    // stale copy so exactly one set survives
    let mut gm = strip_hdr_metadata(gainmap);
    let gm_ins: Vec<(u8, Vec<u8>)> = vec![(0xE1, build_secondary_xmp(headroom)), (0xE2, iso.clone())];
    let gm_at = jpeg_app_insert_pos(&gm);
    insert_segments(&mut gm, gm_at, &gm_ins);

    // primary carries the container directory (lengths) + ISO APP2
    let mut out = strip_hdr_metadata(base);
    let base_ins: Vec<(u8, Vec<u8>)> = vec![(0xE1, build_primary_xmp(gm.len())), (0xE2, iso)];
    let base_at = jpeg_app_insert_pos(&out);
    insert_segments(&mut out, base_at, &base_ins);

    let mpf_at = jpeg_app_insert_pos(&out);
    let base_total_len = out.len() + MPF_MARKER_LEN;
    let gm_offset = base_total_len - (mpf_at + 8);
    let mpf = build_mpf_segment(base_total_len, gm.len(), gm_offset);
    out.splice(mpf_at..mpf_at, mpf);
    out.extend_from_slice(&gm);
    Ok(out)
}

#[wasm_bindgen]
pub fn apple_hdr_iso_gainmap(gainmap: Vec<u8>, gm_w: usize, gm_h: usize, headroom: f64) -> Result<Vec<u8>, JsValue> {
    apple_hdr_iso_gainmap_inner(&gainmap, gm_w, gm_h, headroom).map_err(|e| JsValue::from_str(&e))
}

#[wasm_bindgen]
pub fn ultrahdr_create(base: Vec<u8>, gainmap: Vec<u8>, headroom: f64) -> Result<Vec<u8>, JsValue> {
    ultrahdr_create_inner(&base, &gainmap, headroom).map_err(|e| JsValue::from_str(&e))
}

// ---- HEIC / Apple HDR gain map ----
// Container parsing to locate the gain map item (Apple aux `auxl`+`auxC` URN or
// ISO 21496-1 `tmap`) plus the Apple MakerNote headroom. The actual gain map
// pixels are decoded by libheif in JS; math follows the MIT reference
// (johncf/apple-hdr-heic), headroom formula per Apple's docs as implemented there.

/// Iterate ISO-BMFF boxes in `data[start..end]`; returns (type, body_start, body_end).
fn bmff_boxes(data: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let end = end.min(data.len());
    let mut pos = start;
    while pos + 8 <= end {
        let size32 = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as u64;
        let typ = [data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]];
        let (hdr, size) = if size32 == 1 {
            if pos + 16 > end {
                break;
            }
            let hi = u32::from_be_bytes([data[pos + 8], data[pos + 9], data[pos + 10], data[pos + 11]]) as u64;
            let lo = u32::from_be_bytes([data[pos + 12], data[pos + 13], data[pos + 14], data[pos + 15]]) as u64;
            (16usize, (hi << 32) | lo)
        } else if size32 == 0 {
            (8usize, (end - pos) as u64)
        } else {
            (8usize, size32)
        };
        if size < hdr as u64 {
            break;
        }
        let body_end = (pos + size as usize).min(end);
        out.push((typ, pos + hdr, body_end));
        pos += size as usize;
    }
    out
}

fn be_u16(data: &[u8], off: usize) -> Option<u16> {
    if off + 2 <= data.len() {
        Some(u16::from_be_bytes([data[off], data[off + 1]]))
    } else {
        None
    }
}

// (item_id, item_type); item_type is [0;4] for infe v0/v1
fn heic_items(meta: &[u8]) -> Vec<(u16, [u8; 4])> {
    let mut items = Vec::new();
    for (typ, body, end) in bmff_boxes(meta, 0, meta.len()) {
        if typ != *b"iinf" || body + 4 > end {
            continue;
        }
        let version = meta[body];
        let mut pos = body + 4;
        let count = if version == 0 {
            let c = be_u16(meta, pos).unwrap_or(0) as usize;
            pos += 2;
            c
        } else {
            let c = match u32be(meta, pos) {
                Some(c) => c,
                None => break,
            };
            pos += 4;
            c
        };
        let _ = count;
        for (t2, b2, e2) in bmff_boxes(meta, pos, end) {
            if t2 != *b"infe" || b2 + 4 > e2 {
                continue;
            }
            let ver = meta[b2];
            let mut p = b2 + 4;
            let id = be_u16(meta, p).unwrap_or(0);
            p += 2;
            p += 2; // protection index
            let mut itype = [0u8; 4];
            if ver >= 2 && p + 4 <= e2 {
                itype.copy_from_slice(&meta[p..p + 4]);
            }
            items.push((id, itype));
        }
    }
    items
}

type RefPairs = Vec<([u8; 4], Vec<(u16, Vec<u16>)>)>;

// reference pairs per type: type -> [(from, [to; n])]
fn heic_references(meta: &[u8]) -> RefPairs {
    let mut out = Vec::new();
    for (typ, body, end) in bmff_boxes(meta, 0, meta.len()) {
        if typ != *b"iref" || body + 4 > end {
            continue;
        }
        for (rtyp, rb, re) in bmff_boxes(meta, body + 4, end) {
            let from = match be_u16(meta, rb) {
                Some(v) => v,
                None => continue,
            };
            let n = match be_u16(meta, rb + 2) {
                Some(v) => v as usize,
                None => continue,
            };
            let mut tos = Vec::new();
            for i in 0..n {
                if let Some(t) = be_u16(meta, rb + 4 + i * 2) {
                    tos.push(t);
                }
            }
            if re >= rb + 4 {
                out.push((rtyp, vec![(from, tos)]));
            }
        }
    }
    out
}

fn heic_primary_item(meta: &[u8]) -> Option<u16> {
    for (typ, body, _) in bmff_boxes(meta, 0, meta.len()) {
        if typ == *b"pitm" && body + 4 <= meta.len() {
            let version = meta[body];
            if version == 0 {
                return be_u16(meta, body + 4);
            }
            return u32be(meta, body + 4).map(|v| v as u16);
        }
    }
    None
}

type PropBox = ([u8; 4], usize, usize);
type ItemProps = (Vec<PropBox>, Vec<(u16, Vec<u16>)>);

// ipco property boxes (type + absolute body range) in 1-based order, and
// ipma associations item_id -> property indices
fn heic_item_properties(meta: &[u8]) -> ItemProps {
    let mut props: Vec<PropBox> = Vec::new();
    let mut assoc: Vec<(u16, Vec<u16>)> = Vec::new();
    for (typ, body, end) in bmff_boxes(meta, 0, meta.len()) {
        if typ != *b"iprp" {
            continue;
        }
        for (t2, b2, e2) in bmff_boxes(meta, body, end) {
            if t2 == *b"ipco" {
                props.extend(bmff_boxes(meta, b2, e2));
            } else if t2 == *b"ipma" && b2 + 4 <= e2 {
                // malformed ipma: keep whatever entries parsed so far
                let _ = (|| -> Option<()> {
                    let version = meta[b2];
                    let flags = meta[b2 + 3];
                    let count = u32be(meta, b2 + 4)?;
                    let mut p = b2 + 8;
                    for _ in 0..count {
                        let item_id = if version < 1 {
                            let v = be_u16(meta, p)?;
                            p += 2;
                            v
                        } else {
                            let v = u32be(meta, p)?;
                            p += 4;
                            v as u16
                        };
                        let n = if flags & 1 != 0 {
                            let v = be_u16(meta, p)?;
                            p += 2;
                            v as usize
                        } else {
                            let v = *meta.get(p)?;
                            p += 1;
                            v as usize
                        };
                        let mut indices = Vec::with_capacity(n);
                        for _ in 0..n {
                            if flags & 1 != 0 {
                                let v = be_u16(meta, p)?;
                                p += 2;
                                indices.push(v & 0x7fff);
                            } else {
                                let v = *meta.get(p)?;
                                p += 1;
                                indices.push((v & 0x7f) as u16);
                            }
                        }
                        assoc.push((item_id, indices));
                    }
                    Some(())
                })();
            }
        }
    }
    (props, assoc)
}

#[derive(Serialize)]
struct AppleHeicInfo {
    ok: bool,
    kind: String,
    gainmap_item_id: u32,
    headroom: f64,
}

// Apple's documented headroom derivation (see johncf/apple-hdr-heic, MIT)
fn apple_headroom(maker33: f64, maker48: f64) -> f64 {
    let stops = if maker33 < 1.0 {
        if maker48 <= 0.01 {
            -20.0 * maker48 + 1.8
        } else {
            -0.101 * maker48 + 1.601
        }
    } else if maker48 <= 0.01 {
        -70.0 * maker48 + 3.0
    } else {
        -0.303 * maker48 + 2.303
    };
    2f64.powf(stops.max(0.0))
}

// Apple maker note: "Apple iOS\0" + version/flags(3) + "MM"/"II" + IFD count(u16)
// + 12-byte entries; value offsets are relative to the maker note start.
fn parse_apple_makernote(mn: &[u8]) -> Option<(f64, f64)> {
    if mn.len() < 20 {
        return None;
    }
    let be = match &mn[12..14] {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    let u16at = |o: usize| -> Option<u16> {
        if o + 2 <= mn.len() {
            Some(if be { u16::from_be_bytes([mn[o], mn[o + 1]]) } else { u16::from_le_bytes([mn[o], mn[o + 1]]) })
        } else {
            None
        }
    };
    let u32at = |o: usize| -> Option<u32> {
        if o + 4 <= mn.len() {
            Some(if be {
                u32::from_be_bytes([mn[o], mn[o + 1], mn[o + 2], mn[o + 3]])
            } else {
                u32::from_le_bytes([mn[o], mn[o + 1], mn[o + 2], mn[o + 3]])
            })
        } else {
            None
        }
    };
    let count = u16at(14)? as usize;
    if count == 0 || count > 200 || 16 + count * 12 > mn.len() {
        return None;
    }
    let (mut headroom, mut gain) = (None, None);
    for k in 0..count {
        let p = 16 + k * 12;
        let tag = u16at(p)?;
        let typ = u16at(p + 2)?;
        let cnt = u32at(p + 4)?;
        let val = u32at(p + 8)? as usize;
        if (tag == 0x21 || tag == 0x30) && typ == 10 && cnt == 1 && val + 8 <= mn.len() {
            let num = u32at(val)? as f64;
            let den = u32at(val + 4)? as f64;
            if den == 0.0 {
                continue;
            }
            if tag == 0x21 {
                headroom = Some(num / den);
            } else {
                gain = Some(num / den);
            }
        }
    }
    Some((headroom?, gain?))
}

fn apple_makernote_values(raw: &[u8]) -> Option<(f64, f64)> {
    let magic: &[u8] = b"Apple iOS\0";
    let mut at = 0usize;
    while let Some(i) = raw[at..].windows(magic.len()).position(|w| w == magic).map(|p| p + at) {
        if let Some(v) = parse_apple_makernote(&raw[i..]) {
            return Some(v);
        }
        at = i + magic.len();
    }
    None
}

fn heic_apple_info_inner(raw: &[u8]) -> AppleHeicInfo {
    let none = AppleHeicInfo { ok: false, kind: String::new(), gainmap_item_id: 0, headroom: 0.0 };
    let (meta_start, meta_end) = match bmff_meta_range(raw) {
        Some(r) => r,
        None => return none,
    };
    let meta = &raw[meta_start..meta_end];
    let items = heic_items(meta);
    let refs = heic_references(meta);
    let primary = heic_primary_item(meta);
    let (props, assoc) = heic_item_properties(meta);

    // auxiliary-image type URN declared by an item's `auxC` property
    let auxc_of = |item_id: u16| -> Option<&[u8]> {
        let indices = assoc.iter().find(|(id, _)| *id == item_id)?.1.clone();
        for idx in indices {
            let (ptyp, pb, pe) = *props.get((idx as usize).checked_sub(1)?)?;
            if ptyp != *b"auxC" || pb + 4 > pe {
                continue;
            }
            let urn_start = pb + 4;
            let urn_end = meta[urn_start..pe].iter().position(|&b| b == 0).map(|l| urn_start + l).unwrap_or(pe);
            return Some(&meta[urn_start..urn_end]);
        }
        None
    };

    let mut result = none;
    // Apple aux gain map: `auxl` reference to the primary, auxC URN mentions hdrgainmap
    if let Some(primary) = primary {
        'outer: for (rtyp, pairs) in &refs {
            if rtyp != b"auxl" {
                continue;
            }
            for (from, tos) in pairs {
                if !tos.contains(&primary) {
                    continue;
                }
                let itype = items.iter().find(|(id, _)| id == from).map(|(_, t)| *t).unwrap_or([0; 4]);
                let is_image = itype == *b"hvc1" || itype == *b"grid" || itype == *b"hevc" || itype == *b"av01";
                if !is_image {
                    continue;
                }
                if let Some(urn) = auxc_of(*from) {
                    if contains(urn, b"hdrgainmap") {
                        result.kind = "apple-aux".to_string();
                        result.gainmap_item_id = *from as u32;
                        break 'outer;
                    }
                }
            }
        }
    }
    // ISO 21496-1: `tmap` item referencing base + gain map
    if result.gainmap_item_id == 0 {
        if let Some((tmap_id, _)) = items.iter().find(|(_, t)| t == b"tmap") {
            for (rtyp, pairs) in &refs {
                if rtyp != b"dimg" {
                    continue;
                }
                for (from, tos) in pairs {
                    if from == tmap_id && tos.len() >= 2 {
                        result.kind = "iso-tmap".to_string();
                        result.gainmap_item_id = tos[1] as u32;
                    }
                }
            }
        }
    }
    // headroom from Apple MakerNote
    if let Some((hr, gain)) = apple_makernote_values(raw) {
        result.headroom = apple_headroom(hr, gain);
    }
    result.ok = result.gainmap_item_id != 0 && result.headroom > 1.0;
    result
}

#[wasm_bindgen]
pub fn heic_apple_info(raw: Vec<u8>) -> JsValue {
    let info = heic_apple_info_inner(&raw);
    <wasm_bindgen::JsValue as JsValueSerdeExt>::from_serde(&info).unwrap()
}

// Display P3 -> BT.2020 linear (D65); row sums = 1 so white is preserved
const P3_TO_BT2020: [[f64; 3]; 3] = [
    [0.7538, 0.2346, 0.0116],
    [0.0458, 0.9409, 0.0133],
    [0.0012, 0.0556, 0.9432],
];

// sRGB EOTF with linear interpolation (1024 steps): the base is 8-bit but the
// gain map is bilinearly resampled, so fractional inputs occur
struct SrgbEotf {
    lut: [f64; 1025],
    lut8: [f64; 256],
}

impl SrgbEotf {
    fn new() -> Self {
        let mut lut = [0.0f64; 1025];
        for (i, v) in lut.iter_mut().enumerate() {
            *v = srgb_to_linear(i as f64 / 1024.0);
        }
        let mut lut8 = [0.0f64; 256];
        for (i, v) in lut8.iter_mut().enumerate() {
            *v = srgb_to_linear(i as f64 / 255.0);
        }
        SrgbEotf { lut, lut8 }
    }

    fn eval8(&self, v: u8) -> f64 {
        self.lut8[v as usize]
    }

    fn eval(&self, v: f64) -> f64 {
        let x = (v.clamp(0.0, 1.0)) * 1024.0;
        let i = x.floor() as usize;
        if i >= 1024 {
            return 1.0;
        }
        let f = x - i as f64;
        self.lut[i] + (self.lut[i + 1] - self.lut[i]) * f
    }
}

// Relative linear luminance (1.0 = SDR reference white = 203 nits) -> PQ code
fn pq_code_from_linear(lin: f64) -> u16 {
    let code = pq_oetf(lin * WATERMARK_SDR_WHITE_NITS) * 65535.0 + 0.5;
    code.clamp(0.0, 65535.0) as u16
}

// Compose an Apple HDR HEIC (base RGBA8 Display P3 + grayscale gain map) into a
// 16-bit PQ PNG (BT.2020, cICP 9/16/0/1) with the watermark banner applied.
// Math per the MIT reference (johncf/apple-hdr-heic):
//   hdr_linear = srgb_eotf(base) * (1 + (headroom - 1) * srgb_eotf(gain))
#[allow(clippy::too_many_arguments)] // wasm 入口按扁平分片传参，inner 与之对应
fn apple_hdr_compose_inner(
    base: &[u8],
    base_w: usize,
    base_h: usize,
    gainmap: &[u8],
    gm_w: usize,
    gm_h: usize,
    headroom: f64,
    mask: &[u8],
    mask_w: usize,
    mask_h: usize,
    off_x: usize,
    off_y: usize,
) -> Result<Vec<u8>, String> {
    if base_w == 0 || base_h == 0 || base.len() < base_w * base_h * 4 {
        return Err("bad base image".to_string());
    }
    if gm_w == 0 || gm_h == 0 || gainmap.len() < gm_w * gm_h * 4 {
        return Err("bad gain map".to_string());
    }
    if mask.len() < mask_w * mask_h * 4 {
        return Err("mask buffer too small".to_string());
    }
    if off_x + mask_w > base_w {
        return Err("mask wider than image".to_string());
    }
    if !headroom.is_finite() || headroom < 1.0 {
        return Err("invalid headroom".to_string());
    }
    let out_h = base_h.max(off_y + mask_h);
    let srgb = SrgbEotf::new();
    let pq = PqEncoder::new();
    let scale_range = headroom - 1.0;
    let white = watermark_codes([255, 255, 255], PngTransfer::Pq, PngPrimaries::Bt2020, true);

    let mut encoded: Vec<u8> = Vec::new();
    {
        use std::io::Write as _;
        let mut encoder = png::Encoder::new(&mut encoded, base_w as u32, out_h as u32);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Sixteen);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| format!("encode: {e}"))?;
        let mut stream = writer.stream_writer().map_err(|e| format!("encode: {e}"))?;
        let mut row = vec![0u8; base_w * 6];
        for y in 0..out_h {
            let mask_y = if y >= off_y && y < off_y + mask_h { Some(y - off_y) } else { None };
            if y >= base_h {
                // extension below the photo: reference white, then the watermark
                // mask is composited on top (it may span this area)
                let mut i = 0usize;
                for x in 0..base_w {
                    let mut codes = [white[0] as u16, white[1] as u16, white[2] as u16];
                    if let Some(my) = mask_y {
                        if x >= off_x && x < off_x + mask_w {
                            let mi = (my * mask_w + (x - off_x)) * 4;
                            let a = mask[mi + 3] as u32;
                            if a > 0 {
                                let inv = 255 - a;
                                let wmc = watermark_codes([mask[mi], mask[mi + 1], mask[mi + 2]], PngTransfer::Pq, PngPrimaries::Bt2020, true);
                                for c in 0..3 {
                                    let src = codes[c] as u32;
                                    codes[c] = ((src * inv + wmc[c] * a + 127) / 255) as u16;
                                }
                            }
                        }
                    }
                    for code in &codes {
                        let b = code.to_be_bytes();
                        row[i] = b[0];
                        row[i + 1] = b[1];
                        i += 2;
                    }
                }
            } else {
                let base_row = &base[y * base_w * 4..(y + 1) * base_w * 4];
                let gy = ((y as f64 + 0.5) * gm_h as f64 / base_h as f64 - 0.5).max(0.0);
                let gy0 = gy.floor() as usize;
                let gy1 = (gy0 + 1).min(gm_h - 1);
                let fy = (gy - gy0 as f64).clamp(0.0, 1.0);
                let mut i = 0usize;
                for x in 0..base_w {
                    let px = &base_row[x * 4..x * 4 + 4];
                    let p3 = [srgb.eval8(px[0]), srgb.eval8(px[1]), srgb.eval8(px[2])];
                    let gx = ((x as f64 + 0.5) * gm_w as f64 / base_w as f64 - 0.5).max(0.0);
                    let gx0 = gx.floor() as usize;
                    let gx1 = (gx0 + 1).min(gm_w - 1);
                    let fx = (gx - gx0 as f64).clamp(0.0, 1.0);
                    let g00 = gainmap[(gy0 * gm_w + gx0) * 4] as f64 / 255.0;
                    let g01 = gainmap[(gy0 * gm_w + gx1) * 4] as f64 / 255.0;
                    let g10 = gainmap[(gy1 * gm_w + gx0) * 4] as f64 / 255.0;
                    let g11 = gainmap[(gy1 * gm_w + gx1) * 4] as f64 / 255.0;
                    let g = (g00 + (g01 - g00) * fx) + ((g10 + (g11 - g10) * fx) - (g00 + (g01 - g00) * fx)) * fy;
                    let scale = 1.0 + scale_range * srgb.eval(g);
                    let mut codes = [0u16; 3];
                    for c in 0..3 {
                        let lin = (P3_TO_BT2020[c][0] * p3[0] + P3_TO_BT2020[c][1] * p3[1] + P3_TO_BT2020[c][2] * p3[2]) * scale;
                        codes[c] = pq.code(lin);
                    }
                    if let Some(my) = mask_y {
                        if x >= off_x && x < off_x + mask_w {
                            let mi = (my * mask_w + (x - off_x)) * 4;
                            let a = mask[mi + 3] as u32;
                            if a > 0 {
                                let inv = 255 - a;
                                let wmc = watermark_codes([mask[mi], mask[mi + 1], mask[mi + 2]], PngTransfer::Pq, PngPrimaries::Bt2020, true);
                                for c in 0..3 {
                                    let src = codes[c] as u32;
                                    codes[c] = ((src * inv + wmc[c] * a + 127) / 255) as u16;
                                }
                            }
                        }
                    }
                    for code in &codes {
                        let b = code.to_be_bytes();
                        row[i] = b[0];
                        row[i + 1] = b[1];
                        i += 2;
                    }
                }
            }
            stream.write_all(&row).map_err(|e| format!("encode: {e}"))?;
        }
        stream.finish().map_err(|e| format!("encode: {e}"))?;
    }

    let chunks = parse_png_chunks(&encoded)?;
    let ihdr = chunks.iter().find(|c| c.typ == *b"IHDR").ok_or("encoder produced no IHDR")?;
    let mut out = Vec::with_capacity(encoded.len() + 64);
    out.extend_from_slice(&PNG_SIG);
    write_chunk(&mut out, b"IHDR", ihdr.data);
    // BT.2020 primaries, PQ transfer, matrix 0 (RGB), full range
    write_chunk(&mut out, b"cICP", &[9, 16, 0, 1]);
    for c in chunks.iter().filter(|c| c.typ == *b"IDAT") {
        write_chunk(&mut out, b"IDAT", c.data);
    }
    write_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

#[allow(clippy::too_many_arguments)] // wasm 入口按扁平分片传参
#[wasm_bindgen]
pub fn apple_hdr_compose_png(
    base: Vec<u8>,
    base_w: u32,
    base_h: u32,
    gainmap: Vec<u8>,
    gm_w: u32,
    gm_h: u32,
    headroom: f64,
    mask: Vec<u8>,
    mask_w: u32,
    mask_h: u32,
    off_x: u32,
    off_y: u32,
) -> Result<Vec<u8>, JsValue> {
    apple_hdr_compose_inner(
        &base,
        base_w as usize,
        base_h as usize,
        &gainmap,
        gm_w as usize,
        gm_h as usize,
        headroom,
        &mask,
        mask_w as usize,
        mask_h as usize,
        off_x as usize,
        off_y as usize,
    )
    .map_err(|e| JsValue::from_str(&e))
}

// ---- PNG high-fidelity compositing ----
// Decode at native bit depth, blend an RGBA watermark mask in the source's
// gamma domain (identical math to CSS/canvas alpha compositing), re-encode at
// the same bit depth, and pass through all ancillary chunks (iCCP/cICP/XMP/…)
// byte-for-byte.

const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

struct PngChunk<'a> {
    typ: [u8; 4],
    data: &'a [u8],
}

fn parse_png_chunks(data: &[u8]) -> Result<Vec<PngChunk<'_>>, String> {
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
        chunks.push(PngChunk { typ, data: &data[off + 8..off + 8 + len] });
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

// ---- HDR PNG watermark encoding ----
// The banner mask is authored against the BT.2408 HDR reference white (203 nits;
// 75% signal in HLG). For HDR canvases the watermark is encoded in the target's
// primaries: mask codes are sRGB (JS renders the mask as sRGB for HDR sources),
// converted to the source's primaries before the PQ/HLG transfer. Photo pixels
// are untouched.

const WATERMARK_SDR_WHITE_NITS: f64 = 203.0;
// BT.2408: HDR reference white (diffuse white) sits at 75% HLG signal
const HLG_REFERENCE_WHITE_SIGNAL: f64 = 0.75;

#[derive(Clone, Copy, PartialEq)]
enum PngPrimaries {
    Bt709,
    P3,
    Bt2020,
}

#[derive(Clone, Copy, PartialEq)]
enum PngTransfer {
    Sdr,
    Pq,
    Hlg,
}

fn png_transfer(chunks: &[PngChunk]) -> PngTransfer {
    if let Some(c) = chunks.iter().find(|c| c.typ == *b"cICP") {
        return match c.data.get(1).copied() {
            Some(16) => PngTransfer::Pq,
            Some(18) => PngTransfer::Hlg,
            _ => PngTransfer::Sdr,
        };
    }
    // mDCv/cLLi without cICP: static HDR metadata, PQ is the de-facto transfer
    if chunks.iter().any(|c| c.typ == *b"mDCv" || c.typ == *b"cLLi") {
        return PngTransfer::Pq;
    }
    PngTransfer::Sdr
}

fn png_primaries(chunks: &[PngChunk]) -> PngPrimaries {
    match chunks.iter().find(|c| c.typ == *b"cICP").and_then(|c| c.data.first()).copied() {
        Some(1) => PngPrimaries::Bt709,
        Some(12) => PngPrimaries::P3,
        _ => PngPrimaries::Bt2020,
    }
}

fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

const PQ_M1: f64 = 2610.0 / 16384.0;
const PQ_M2: f64 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0;

// ST.2084 (PQ) OETF: absolute luminance in nits → normalized code [0, 1]
fn pq_oetf(nits: f64) -> f64 {
    let y = (nits / 10000.0).max(0.0);
    let ym = y.powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * ym) / (1.0 + PQ_C3 * ym)).powf(PQ_M2)
}

// Fast PQ encoder for the Apple HDR path: per-pixel powf is too slow, so
// v -> v^m1 is tabulated over u = v^(1/8) (three hardware sqrts make the
// interpolated function near-linear) and the final X^m2 over Ym.
struct PqEncoder {
    umax: f64,
    k: f64,
    ym_max: f64,
    vm1: Vec<f64>,
    g: Vec<f64>,
}

impl PqEncoder {
    fn new() -> Self {
        const VMAX: f64 = 32.0;
        const N: usize = 2048;
        let umax = VMAX.powf(0.125);
        let mut vm1 = Vec::with_capacity(N + 1);
        for i in 0..=N {
            let u = i as f64 / N as f64 * umax;
            let v = u * u * u * u * u * u * u * u;
            vm1.push(v.powf(PQ_M1));
        }
        let ym_max = (0.0203 * VMAX).powf(PQ_M1);
        let mut g = Vec::with_capacity(N + 1);
        for i in 0..=N {
            let ym = i as f64 / N as f64 * ym_max;
            g.push(((PQ_C1 + PQ_C2 * ym) / (1.0 + PQ_C3 * ym)).powf(PQ_M2) * 65535.0);
        }
        PqEncoder { umax, k: 0.0203f64.powf(PQ_M1), ym_max, vm1, g }
    }

    fn code(&self, v: f64) -> u16 {
        let v = v.max(0.0);
        if v >= 32.0 {
            return pq_code_from_linear(v);
        }
        let u = (v.sqrt()).sqrt().sqrt();
        let n = (self.vm1.len() - 1) as f64;
        // v^m1
        let p = (u / self.umax * n).min(n);
        let i = p.floor() as usize;
        let f = p - i as f64;
        let vm = self.vm1[i] + (self.vm1[(i + 1).min(self.vm1.len() - 1)] - self.vm1[i]) * f;
        // X^m2 (code)
        let ym = self.k * vm;
        let p2 = (ym / self.ym_max * n).clamp(0.0, n);
        let j = p2.floor() as usize;
        let f2 = p2 - j as f64;
        let code = self.g[j] + (self.g[(j + 1).min(self.g.len() - 1)] - self.g[j]) * f2;
        (code + 0.5).clamp(0.0, 65535.0) as u16
    }
}

// BT.2100 HLG OETF (scene linear → signal), piecewise at E = 1/12 / signal 0.5
fn hlg_oetf(e: f64) -> f64 {
    const A: f64 = 0.17883277;
    const B: f64 = 0.28466892;
    const C: f64 = 0.55991073;
    if e <= 1.0 / 12.0 {
        (3.0 * e).sqrt()
    } else {
        A * (12.0 * e - B).ln() + C
    }
}

fn hlg_inverse_oetf(v: f64) -> f64 {
    const A: f64 = 0.17883277;
    const B: f64 = 0.28466892;
    const C: f64 = 0.55991073;
    if v <= 0.5 {
        v * v / 3.0
    } else {
        (((v - C) / A).exp() + B) / 12.0
    }
}

// Linear sRGB → linear target primaries (D65), row sums = 1 (white preserved)
fn srgb_to_target_matrix(p: PngPrimaries) -> [[f64; 3]; 3] {
    match p {
        PngPrimaries::Bt709 => [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ],
        PngPrimaries::P3 => [
            [0.822462, 0.177538, 0.0],
            [0.033194, 0.966806, 0.0],
            [0.017083, 0.072397, 0.910520],
        ],
        PngPrimaries::Bt2020 => [
            [0.627404, 0.329283, 0.0433136],
            [0.069097, 0.919540, 0.0113623],
            [0.0163916, 0.0880132, 0.895595],
        ],
    }
}

fn straight_code(mask8: u8, sixteen: bool) -> u32 {
    if sixteen {
        mask8 as u32 * 257
    } else {
        mask8 as u32
    }
}

fn watermark_codes(mask_px: [u8; 3], transfer: PngTransfer, primaries: PngPrimaries, sixteen: bool) -> [u32; 3] {
    let scale = if sixteen { 65535u32 } else { 255u32 };
    if transfer == PngTransfer::Sdr {
        return [
            straight_code(mask_px[0], sixteen),
            straight_code(mask_px[1], sixteen),
            straight_code(mask_px[2], sixteen),
        ];
    }
    let m = srgb_to_target_matrix(primaries);
    let lin = [
        srgb_to_linear(mask_px[0] as f64 / 255.0),
        srgb_to_linear(mask_px[1] as f64 / 255.0),
        srgb_to_linear(mask_px[2] as f64 / 255.0),
    ];
    let mut out = [0u32; 3];
    for c in 0..3 {
        let rel = m[c][0] * lin[0] + m[c][1] * lin[1] + m[c][2] * lin[2];
        let code_norm = match transfer {
            PngTransfer::Pq => pq_oetf(rel * WATERMARK_SDR_WHITE_NITS),
            PngTransfer::Hlg => hlg_oetf(rel * hlg_inverse_oetf(HLG_REFERENCE_WHITE_SIGNAL)),
            PngTransfer::Sdr => 0.0,
        };
        let code = (code_norm * scale as f64 + 0.5).floor();
        out[c] = (code as u32).min(scale);
    }
    out
}

// The png crate rejects cICP with matrix_coefficients != 0 (PNG is RGB-only),
// but real-world PQ PNGs often carry 9 (BT.2020 NCLX). Decode a copy with the
// matrix zeroed; the output chunk is normalized the same way (matrix is
// meaningless for RGB, bytes for primaries/transfer are preserved).
fn sanitized_for_decode(original: &[u8], chunks: &[PngChunk]) -> Option<Vec<u8>> {
    let needs = chunks
        .iter()
        .any(|c| c.typ == *b"cICP" && c.data.len() >= 3 && c.data[2] != 0);
    if !needs {
        return None;
    }
    let mut out = Vec::with_capacity(original.len());
    out.extend_from_slice(&PNG_SIG);
    for c in chunks {
        if c.typ == *b"cICP" && c.data.len() >= 3 && c.data[2] != 0 {
            let mut data = c.data.to_vec();
            data[2] = 0;
            write_chunk(&mut out, &c.typ, &data);
        } else {
            write_chunk(&mut out, &c.typ, c.data);
        }
    }
    Some(out)
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

    // Decode at native bit depth (EXPAND: tRNS→alpha, <8bit gray→8bit).
    // Decode straight into the final canvas buffer: extension rows are filled
    // beforehand, so no separate decode buffer + copy is needed.
    let decode_owned = sanitized_for_decode(original, &chunks);
    let decode_src: &[u8] = decode_owned.as_deref().unwrap_or(original);
    let mut decoder = png::Decoder::new(std::io::Cursor::new(decode_src));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| format!("decode: {e}"))?;
    let frame_bytes = reader.output_buffer_size().ok_or_else(|| "bad PNG geometry".to_string())?;
    let (out_ct, out_depth) = reader.output_color_type();
    let (w, h) = (reader.info().width as usize, reader.info().height as usize);
    let sixteen = out_depth == png::BitDepth::Sixteen;
    let channels = out_ct.samples();
    let bps = channels * if sixteen { 2 } else { 1 };

    if mask.len() < mask_w * mask_h * 4 {
        return Err("mask buffer too small".to_string());
    }
    if off_x + mask_w > w {
        return Err("mask wider than image".to_string());
    }
    // Vertical extension: watermark banner below the photo extends the canvas
    // (gap/pad rows filled with the source's reference white, 203-nit PQ for HDR)
    let transfer = png_transfer(&chunks);
    let primaries = png_primaries(&chunks);
    let white_codes = watermark_codes([255, 255, 255], transfer, primaries, sixteen);
    let out_h = h.max(off_y + mask_h);
    let row_bytes = w * bps;
    let mut canvas: Vec<u8> = if out_h > h {
        let mut row = vec![0xFFu8; row_bytes];
        if transfer != PngTransfer::Sdr {
            let mut i = 0usize;
            for _ in 0..w {
                for c in 0..channels {
                    let code = if c == 3 { if sixteen { 65535 } else { 255 } } else { white_codes[c.min(2)] };
                    if sixteen {
                        let b = (code as u16).to_be_bytes();
                        row[i] = b[0];
                        row[i + 1] = b[1];
                        i += 2;
                    } else {
                        row[i] = code as u8;
                        i += 1;
                    }
                }
            }
        }
        let mut c = Vec::with_capacity(out_h * row_bytes);
        c.resize(h * row_bytes, 0);
        for _ in h..out_h {
            c.extend_from_slice(&row);
        }
        c
    } else {
        vec![0u8; frame_bytes]
    };
    let info = reader.next_frame(&mut canvas[..frame_bytes]).map_err(|e| format!("decode: {e}"))?;
    debug_assert_eq!(info.buffer_size(), frame_bytes);

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
            // grayscale target uses mask luma; RGB(A) uses matching channel
            let mask_px = if channels >= 3 {
                [mask[mi], mask[mi + 1], mask[mi + 2]]
            } else {
                let l = ((mask[mi] as u32 * 77 + mask[mi + 1] as u32 * 150 + mask[mi + 2] as u32 * 29) >> 8) as u8;
                [l, l, l]
            };
            let codes = watermark_codes(mask_px, transfer, primaries, sixteen);
            for c in 0..channels {
                let wm_code = if c == 3 { straight_code(mask[mi + 2], sixteen) } else { codes[c.min(2)] };
                if sixteen {
                    let src = u16::from_be_bytes([canvas[di + c * 2], canvas[di + c * 2 + 1]]) as u32;
                    let out = (src * inv + wm_code * a + 127) / 255;
                    let outb = (out as u16).to_be_bytes();
                    canvas[di + c * 2] = outb[0];
                    canvas[di + c * 2 + 1] = outb[1];
                } else {
                    let src = canvas[di + c] as u32;
                    canvas[di + c] = ((src * inv + wm_code * a + 127) / 255) as u8;
                }
            }
        }
    }

    // Re-encode (height may be extended; color type/depth preserved)
    // Re-encode (height may be extended; color type/depth preserved).
    // Streaming writer filters row by row: avoids a second full-image buffer.
    let mut encoded: Vec<u8> = Vec::with_capacity(canvas.len() * 3 / 4);
    {
        use std::io::Write as _;
        let mut encoder = png::Encoder::new(&mut encoded, info.width, out_h as u32);
        encoder.set_color(info.color_type);
        encoder.set_depth(info.bit_depth);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| format!("encode: {e}"))?;
        let mut stream = writer.stream_writer().map_err(|e| format!("encode: {e}"))?;
        stream.write_all(&canvas).map_err(|e| format!("encode: {e}"))?;
        stream.finish().map_err(|e| format!("encode: {e}"))?;
    }
    drop(canvas);
    let enc_chunks = parse_png_chunks(&encoded)?;

    // Assemble: sig + encoder IHDR (authoritative: geometry/interlace) + passthrough ancillary + IDAT* + IEND
    let enc_ihdr = enc_chunks.iter().find(|c| c.typ == *b"IHDR").ok_or("encoder produced no IHDR")?;
    let mut out = Vec::with_capacity(original.len() / 2 + encoded.len());
    out.extend_from_slice(&PNG_SIG);
    write_chunk(&mut out, b"IHDR", enc_ihdr.data);
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
        // matrix_coefficients is meaningless for RGB PNG and must be 0 per spec;
        // normalize so the output stays decodable (same as sanitized_for_decode)
        if c.typ == *b"cICP" && c.data.len() >= 3 && c.data[2] != 0 {
            let mut data = c.data.to_vec();
            data[2] = 0;
            write_chunk(&mut out, &c.typ, &data);
            continue;
        }
        write_chunk(&mut out, &c.typ, c.data);
    }
    for c in enc_chunks.iter().filter(|c| c.typ == *b"IDAT") {
        write_chunk(&mut out, b"IDAT", c.data);
    }
    write_chunk(&mut out, b"IEND", &[]);
    drop(enc_chunks);
    drop(encoded);
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
                    data.extend_from_slice(&(fill + c).to_be_bytes());
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
        // 已知值 0x1234 编码后，经 crate 解码应还原（crate 对 16bit 为文件字节序原样透传）
        let src = encode_test_png(2, 2, true, false, 0x1234);
        let decoder = png::Decoder::new(std::io::Cursor::new(&src));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        assert_eq!(info.bit_depth, png::BitDepth::Sixteen);
        let first = u16::from_be_bytes([buf[0], buf[1]]);
        assert_eq!(first, 0x1234, "png crate 16bit samples stay in file byte order (big-endian)");
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
                        let v = u16::from_be_bytes([ob[di + c * 2], ob[di + c * 2 + 1]]);
                        if v != 65535 {
                            bad_inside += 1;
                            break;
                        }
                    }
                } else if y == oy {
                    // alpha=128 红：R = (src+65535*... )/255 公式验证一行
                    checked_inside += 1;
                    for c in 0..3 {
                        let s = u16::from_be_bytes([sb[di + c * 2], sb[di + c * 2 + 1]]) as u32;
                        let wm = if c == 0 { 255u32 * 257 } else { 0 };
                        let expect = ((s * 127 + wm * 128 + 127) / 255) as u16;
                        let got = u16::from_be_bytes([ob[di + c * 2], ob[di + c * 2 + 1]]);
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

    // ---- HDR PNG watermark mapping tests ----

    fn with_cicp(png: Vec<u8>, data: [u8; 4]) -> Vec<u8> {
        let chunks = parse_png_chunks(&png).unwrap();
        let mut out = Vec::new();
        out.extend_from_slice(&PNG_SIG);
        for c in &chunks {
            write_chunk(&mut out, &c.typ, c.data);
            if c.typ == *b"IHDR" {
                write_chunk(&mut out, b"cICP", &data);
            }
        }
        out
    }

    fn decode_png_buf(data: &[u8]) -> (png::OutputInfo, Vec<u8>) {
        let decoder = png::Decoder::new(std::io::Cursor::new(data));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf.truncate(info.buffer_size());
        (info, buf)
    }

    #[test]
    fn pq_oetf_203nit_reference() {
        let v = pq_oetf(WATERMARK_SDR_WHITE_NITS);
        assert!((v - 0.5806888810416109).abs() < 1e-9, "{v}");
        // achromatic invariant: white maps to 203-nit PQ in every primaries set
        for p in [PngPrimaries::Bt709, PngPrimaries::P3, PngPrimaries::Bt2020] {
            assert_eq!(watermark_codes([255; 3], PngTransfer::Pq, p, false), [148; 3]);
            assert_eq!(watermark_codes([255; 3], PngTransfer::Pq, p, true), [38055; 3]);
        }
        assert_eq!(watermark_codes([255; 3], PngTransfer::Sdr, PngPrimaries::Bt2020, false), [255; 3]);
        assert_eq!(watermark_codes([255; 3], PngTransfer::Sdr, PngPrimaries::Bt2020, true), [65535; 3]);
        // saturated red: primaries conversion must be applied (sRGB → target)
        assert_eq!(watermark_codes([255, 0, 0], PngTransfer::Pq, PngPrimaries::Bt2020, true), [34900, 21431, 14422]);
        assert_eq!(watermark_codes([255, 0, 0], PngTransfer::Pq, PngPrimaries::P3, true), [36723, 17660, 14601]);
        assert_eq!(watermark_codes([35; 3], PngTransfer::Pq, PngPrimaries::Bt2020, true), [14530; 3]);
    }

    #[test]
    fn png_cicp_hlg_kind() {
        let png = png_with_chunks(&[png_chunk(b"cICP", &[9, 18, 9, 1])]);
        let info = detect_hdr_inner(&png);
        assert!(info.is_hdr && info.kind == "png-hlg");
    }

    #[test]
    fn png_composite_pq_watermark_maps_to_203nit() {
        let src = with_cicp(encode_test_png(2, 2, true, false, 100), [9, 16, 9, 1]);
        let mut mask = vec![0u8; 2 * 4];
        mask[0..4].copy_from_slice(&[255, 255, 255, 255]);
        mask[4..8].copy_from_slice(&[255, 0, 0, 128]);
        let out = png_composite(&src, &mask, 2, 1, 0, 0).unwrap();
        let (info, buf) = decode_png_buf(&out);
        assert_eq!(info.bit_depth, png::BitDepth::Sixteen);
        let ch = |x: usize, y: usize, c: usize| {
            let i = (y * 2 + x) * 6 + c * 2;
            u32::from(u16::from_be_bytes([buf[i], buf[i + 1]]))
        };
        assert_eq!(ch(0, 0, 0), 38055, "opaque white → 203nit PQ code");
        assert_eq!((ch(1, 0, 0), ch(1, 0, 1), ch(1, 0, 2)), (17568, 10808, 7290), "alpha blend uses sRGB→BT.2020 mapped codes");
        assert_eq!(ch(1, 1, 0), 100, "untouched pixel unchanged");
        assert!(parse_png_chunks(&out).unwrap().iter().any(|c| c.typ == *b"cICP" && c.data == [9, 16, 0, 1]), "cICP matrix normalized to 0");
    }

    #[test]
    fn hlg_reference_white_at_75_percent() {
        assert!((hlg_oetf(hlg_inverse_oetf(HLG_REFERENCE_WHITE_SIGNAL)) - HLG_REFERENCE_WHITE_SIGNAL).abs() < 1e-9);
        // achromatic invariant: reference white maps to the 75% HLG signal
        for p in [PngPrimaries::Bt709, PngPrimaries::P3, PngPrimaries::Bt2020] {
            assert_eq!(watermark_codes([255; 3], PngTransfer::Hlg, p, true), [49151; 3]);
            assert_eq!(watermark_codes([255; 3], PngTransfer::Hlg, p, false), [191; 3]);
        }
        assert_eq!(watermark_codes([35; 3], PngTransfer::Hlg, PngPrimaries::Bt2020, true), [7575; 3]);
        assert_eq!(watermark_codes([255, 0, 0], PngTransfer::Hlg, PngPrimaries::Bt2020, true), [42983, 15359, 7481]);
        assert_eq!(watermark_codes([255, 0, 0], PngTransfer::Hlg, PngPrimaries::P3, true), [46609, 10645, 7637]);
    }

    #[test]
    fn png_composite_hlg_watermark_maps_reference_white() {
        let src = with_cicp(encode_test_png(2, 2, true, false, 100), [9, 18, 9, 1]);
        let mut mask = vec![0u8; 2 * 4];
        mask[0..4].copy_from_slice(&[255, 255, 255, 255]);
        mask[4..8].copy_from_slice(&[255, 0, 0, 128]);
        let out = png_composite(&src, &mask, 2, 1, 0, 0).unwrap();
        let (info, buf) = decode_png_buf(&out);
        assert_eq!(info.bit_depth, png::BitDepth::Sixteen);
        let ch = |x: usize, y: usize, c: usize| {
            let i = (y * 2 + x) * 6 + c * 2;
            u32::from(u16::from_be_bytes([buf[i], buf[i + 1]]))
        };
        assert_eq!(ch(0, 0, 0), 49151, "opaque white → 75% HLG signal");
        assert_eq!((ch(1, 0, 0), ch(1, 0, 1), ch(1, 0, 2)), (21626, 7760, 3806), "alpha blend uses sRGB→BT.2020 mapped codes");
        assert_eq!(ch(1, 1, 0), 100, "untouched pixel unchanged");
        assert!(parse_png_chunks(&out).unwrap().iter().any(|c| c.typ == *b"cICP" && c.data == [9, 18, 0, 1]), "cICP matrix normalized to 0");
    }

    #[test]
    fn png_composite_hlg_extension_fills_reference_white() {
        let src = with_cicp(encode_test_png(2, 2, true, false, 100), [9, 18, 9, 1]);
        let mask = vec![0u8; 2 * 2 * 4];
        let out = png_composite(&src, &mask, 2, 2, 0, 2).unwrap();
        let (info, buf) = decode_png_buf(&out);
        assert_eq!((info.width, info.height), (2, 4));
        let ch = |x: usize, y: usize, c: usize| {
            let i = (y * 2 + x) * 6 + c * 2;
            u32::from(u16::from_be_bytes([buf[i], buf[i + 1]]))
        };
        for y in 2..4 {
            for x in 0..2 {
                for c in 0..3 {
                    assert_eq!(ch(x, y, c), 49151, "extension row must be 75% HLG reference white");
                }
            }
        }
    }

    #[test]
    fn png_composite_hlg_bt709_primaries() {
        let src = with_cicp(encode_test_png(1, 1, false, false, 100), [1, 18, 9, 1]);
        let mask = vec![255u8; 4];
        let out = png_composite(&src, &mask, 1, 1, 0, 0).unwrap();
        let (_, buf) = decode_png_buf(&out);
        assert_eq!(buf[0], 191, "BT.709-primaries HLG still maps reference white to 75% signal");
    }

    #[test]
    fn png_composite_pq_extension_fills_203nit() {
        let src = with_cicp(encode_test_png(2, 2, true, false, 100), [9, 16, 9, 1]);
        let mask = vec![0u8; 2 * 2 * 4];
        let out = png_composite(&src, &mask, 2, 2, 0, 2).unwrap();
        let (info, buf) = decode_png_buf(&out);
        assert_eq!((info.width, info.height), (2, 4));
        let ch = |x: usize, y: usize, c: usize| {
            let i = (y * 2 + x) * 6 + c * 2;
            u32::from(u16::from_be_bytes([buf[i], buf[i + 1]]))
        };
        for y in 2..4 {
            for x in 0..2 {
                for c in 0..3 {
                    assert_eq!(ch(x, y, c), 38055, "extension row must be 203nit PQ white");
                }
            }
        }
        assert_eq!(ch(0, 0, 0), 100, "photo rows unchanged");
    }

    #[test]
    fn png_composite_real_pq_sample_if_present() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test_pictures/synthetic-hdr-pq.png");
        if !path.exists() {
            eprintln!("SKIP: {} not found", path.display());
            return;
        }
        let src = std::fs::read(&path).unwrap();
        let mask = vec![255u8; 4];
        let out = png_composite(&src, &mask, 1, 1, 0, 0).unwrap();
        let (info, buf) = decode_png_buf(&out);
        assert_eq!((info.width, info.height), (1, 1));
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        assert_eq!(buf[0], 148, "8bit PQ code for 203 nits");
        let chunks = parse_png_chunks(&out).unwrap();
        assert!(chunks.iter().any(|c| c.typ == *b"cICP" && c.data == [9, 16, 0, 1]));
        assert!(chunks.iter().any(|c| c.typ == *b"mDCv"), "static HDR metadata passes through");
    }

    #[test]
    fn ultrahdr_assemble_single_gainmap_icc() {
        let Ok(src) = std::fs::read(GOOGLE_FIXTURE) else { eprintln!("SKIP: no google fixture"); return; };
        let (gm_abs, gm_size) = gainmap_range(&src).unwrap();
        let orig_gm = &src[gm_abs..gm_abs + gm_size];
        let base = &src[..gm_abs];
        let orig_icc = jpeg_segments(orig_gm)
            .into_iter()
            .find(|s| s.marker == 0xE2 && s.payload.starts_with(b"ICC_PROFILE\0"))
            .map(|s| s.payload.to_vec())
            .expect("fixture gain map should carry an ICC");
        // simulate a canvas re-encode: the original profile is gone, the encoder
        // attached its own (sRGB) profile instead
        let mut reencoded = strip_icc(orig_gm);
        let fake = [b"ICC_PROFILE\0\x01\x01".as_slice(), b"sRGB fake canvas profile"].concat();
        let at = jpeg_app_insert_pos(&reencoded);
        insert_segments(&mut reencoded, at, &[(0xE2, fake)]);
        let count = |jpeg: &[u8]| {
            jpeg_segments(jpeg).iter().filter(|s| s.marker == 0xE2 && s.payload.starts_with(b"ICC_PROFILE\0")).count()
        };
        assert_eq!(count(&reencoded), 1);

        let out = ultrahdr_assemble_inner(&src, base, &reencoded).unwrap();
        let (out_gm_abs, out_gm_size) = gainmap_range(&out).unwrap();
        let out_gm = &out[out_gm_abs..out_gm_abs + out_gm_size];
        assert_eq!(count(out_gm), 1, "exactly one ICC profile in the output gain map");
        let out_icc = jpeg_segments(out_gm)
            .into_iter()
            .find(|s| s.marker == 0xE2 && s.payload.starts_with(b"ICC_PROFILE\0"))
            .map(|s| s.payload.to_vec())
            .unwrap();
        assert_eq!(out_icc, orig_icc, "original HDR intent profile must survive");
    }

    #[test]
    fn apple_iso_gainmap_round_trips_through_iso_formula() {
        let headroom = 4.915774637972608_f64; // IMG_9519
        let srgb = SrgbEotf::new();
        let denom = headroom.log2();
        let samples: [u8; 5] = [0, 64, 128, 192, 255];
        let mut rgba = Vec::new();
        for s in samples {
            rgba.extend_from_slice(&[s, s, s, 255]);
        }
        let iso = apple_hdr_iso_gainmap_inner(&rgba, samples.len(), 1, headroom).unwrap();
        assert_eq!(iso.len(), samples.len());
        assert_eq!(iso[0], 0);
        assert_eq!(iso[4], 255);
        for (s, v) in samples.iter().zip(iso.iter()) {
            let want = 1.0 + (headroom - 1.0) * srgb.eval(*s as f64 / 255.0);
            let got = 2f64.powf(denom * (*v as f64 / 255.0));
            let rel = (got - want).abs() / want;
            assert!(rel < 0.01, "sample {s}: want {want} got {got}");
        }
    }

    #[test]
    fn ultrahdr_create_writes_consistent_metadata() {
        let Ok(src) = std::fs::read(GOOGLE_FIXTURE) else { eprintln!("SKIP: no google fixture"); return; };
        let (gm_abs, _) = gainmap_range(&src).unwrap();
        let gm = ultrahdr_gainmap_inner(&src).unwrap();
        let out = ultrahdr_create_inner(&src[..gm_abs], &gm, 2.0).unwrap();
        assert_eq!(&out[..2], &[0xFF, 0xD8]);
        let (o_abs, o_size) = gainmap_range(&out).unwrap();
        let out_gm = &out[o_abs..o_abs + o_size];
        assert_eq!(&out_gm[..2], &[0xFF, 0xD8]);
        // primary container directory declares the attached gain map length
        let xmp = jpeg_segments(&out)
            .into_iter()
            .find(|s| s.marker == 0xE1 && s.payload.starts_with(XMP_SIG))
            .expect("primary xmp");
        let text = String::from_utf8_lossy(xmp.payload);
        assert!(text.contains(&format!("Item:Length=\"{o_size}\"")), "container length must match");
        // gain map carries parseable ISO + XMP metadata, neutral value 0
        let iso = jpeg_segments(out_gm)
            .into_iter()
            .find(|s| s.marker == 0xE2 && s.payload.starts_with(ISO21496_URN))
            .expect("iso payload");
        let (multi, values) = iso_neutral_values(iso.payload).expect("iso parses");
        assert!(!multi);
        assert_eq!(values, [0, 0, 0]);
        let gm_xmp = jpeg_segments(out_gm)
            .into_iter()
            .find(|s| s.marker == 0xE1 && s.payload.starts_with(XMP_SIG) && contains(s.payload, b"hdrgm:"))
            .expect("gm xmp");
        let (_, xvalues) = xmp_neutral_values(gm_xmp.payload).expect("xmp parses");
        assert_eq!(xvalues, [0, 0, 0]);
    }

    #[test]
    fn ultrahdr_create_rejects_bad_input() {
        let jpeg = [0xFF, 0xD8, 0xFF, 0xD9];
        assert!(ultrahdr_create_inner(&jpeg, &jpeg, 1.0).is_err());
        assert!(ultrahdr_create_inner(&jpeg, &jpeg, 2.0).is_ok());
        assert!(ultrahdr_create_inner(&[0x00, 0x00], &jpeg, 2.0).is_err());
    }

    // ---- Apple HDR (HEIC) tests ----

    #[test]
    fn apple_headroom_reference_values() {
        // exiftool: IMG_9519 HDRHeadroom=1.540691019 HDRGain=0.0184198767
        let h1 = apple_headroom(1.540691019, 0.0184198767);
        assert!((h1 - 4.915774637972608).abs() < 1e-9, "{h1}");
        // exiftool: IMG_9538 HDRHeadroom=1.489122509 HDRGain=0
        let h2 = apple_headroom(1.489122509, 0.0);
        assert!((h2 - 8.0).abs() < 1e-12, "{h2}");
    }

    #[test]
    fn apple_makernote_synthetic() {
        let mut mn = Vec::new();
        mn.extend_from_slice(b"Apple iOS\0");
        mn.extend_from_slice(&[0, 1]);
        mn.extend_from_slice(b"MM");
        mn.extend_from_slice(&2u16.to_be_bytes()); // IFD count at 14
        // entry 0: tag 0x21 SRATIONAL count 1 value offset 40
        mn.extend_from_slice(&0x0021u16.to_be_bytes());
        mn.extend_from_slice(&10u16.to_be_bytes());
        mn.extend_from_slice(&1u32.to_be_bytes());
        mn.extend_from_slice(&40u32.to_be_bytes());
        // entry 1: tag 0x30 SRATIONAL count 1 value offset 48
        mn.extend_from_slice(&0x0030u16.to_be_bytes());
        mn.extend_from_slice(&10u16.to_be_bytes());
        mn.extend_from_slice(&1u32.to_be_bytes());
        mn.extend_from_slice(&48u32.to_be_bytes());
        mn.extend_from_slice(&29609u32.to_be_bytes());
        mn.extend_from_slice(&19218u32.to_be_bytes());
        mn.extend_from_slice(&4658u32.to_be_bytes());
        mn.extend_from_slice(&252879u32.to_be_bytes());
        let (hr, g) = parse_apple_makernote(&mn).unwrap();
        assert!((hr - 1.540691019).abs() < 1e-9);
        assert!((g - 0.0184198767).abs() < 1e-9);
        assert!((apple_headroom(hr, g) - 4.915774637972608).abs() < 1e-9);
    }

    #[test]
    fn heic_apple_info_real_samples() {
        let cases = [
            ("../test_pictures/IMG_9519.HEIC", 63u32, 4.915774637972608),
            ("../test_pictures/IMG_9538.HEIC", 65u32, 8.0),
        ];
        let mut ran = 0;
        for (p, item_id, headroom) in cases {
            if let Ok(b) = std::fs::read(p) {
                ran += 1;
                let info = heic_apple_info_inner(&b);
                assert!(info.ok, "{p} must parse");
                assert_eq!(info.kind, "apple-aux", "{p} kind");
                assert_eq!(info.gainmap_item_id, item_id, "{p} gain map item id");
                assert!((info.headroom - headroom).abs() < 1e-6, "{p} headroom {} vs {headroom}", info.headroom);
            }
        }
        if ran == 0 {
            eprintln!("SKIP: no local samples");
        }
    }

    fn solid_rgba(w: usize, h: usize, rgb: [u8; 3], a: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 4);
        for _ in 0..w * h {
            v.extend_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
        }
        v
    }

    fn px16(buf: &[u8], w: usize, x: usize, y: usize, c: usize) -> u16 {
        let i = (y * w + x) * 6 + c * 2;
        u16::from_be_bytes([buf[i], buf[i + 1]])
    }

    #[test]
    fn apple_hdr_compose_reference_codes() {
        let gm0 = solid_rgba(2, 2, [0, 0, 0], 255);
        let gm255 = solid_rgba(2, 2, [255, 255, 255], 255);
        let no_mask = vec![0u8; 4];

        // SDR passthrough: headroom 1 → PQ(203 nits) per channel
        let white = solid_rgba(2, 2, [255, 255, 255], 255);
        let out = apple_hdr_compose_inner(&white, 2, 2, &gm0, 2, 2, 1.0, &no_mask, 1, 1, 0, 0).unwrap();
        let (info, buf) = decode_png_buf(&out);
        assert_eq!(info.bit_depth, png::BitDepth::Sixteen);
        assert_eq!(info.color_type, png::ColorType::Rgb);
        assert_eq!([px16(&buf, 2, 0, 0, 0), px16(&buf, 2, 0, 0, 1), px16(&buf, 2, 0, 0, 2)], [38055; 3]);

        let red = solid_rgba(2, 2, [255, 0, 0], 255);
        let out = apple_hdr_compose_inner(&red, 2, 2, &gm0, 2, 2, 1.0, &no_mask, 1, 1, 0, 0).unwrap();
        let (_, buf) = decode_png_buf(&out);
        assert_eq!([px16(&buf, 2, 0, 0, 0), px16(&buf, 2, 0, 0, 1), px16(&buf, 2, 0, 0, 2)], [36133, 19266, 5867]);

        let gray = solid_rgba(2, 2, [100, 100, 100], 255);
        let out = apple_hdr_compose_inner(&gray, 2, 2, &gm0, 2, 2, 1.0, &no_mask, 1, 1, 0, 0).unwrap();
        let (_, buf) = decode_png_buf(&out);
        assert_eq!(px16(&buf, 2, 0, 0, 0), 24876);

        // Full gain: 203 * headroom nits
        let out = apple_hdr_compose_inner(&white, 2, 2, &gm255, 2, 2, 4.0, &no_mask, 1, 1, 0, 0).unwrap();
        let (_, buf) = decode_png_buf(&out);
        assert_eq!(px16(&buf, 2, 0, 0, 0), 47785);
        let out = apple_hdr_compose_inner(&white, 2, 2, &gm255, 2, 2, 4.915774637972608, &no_mask, 1, 1, 0, 0).unwrap();
        let (_, buf) = decode_png_buf(&out);
        assert_eq!(px16(&buf, 2, 0, 0, 0), 49256);
    }

    #[test]
    fn apple_hdr_compose_watermark_extension_cicp() {
        let base = solid_rgba(2, 2, [100, 100, 100], 255);
        let gm0 = solid_rgba(2, 2, [0, 0, 0], 255);

        // overlay: opaque white banner on the photo's last row
        let mask = solid_rgba(2, 1, [255, 255, 255], 255);
        let out = apple_hdr_compose_inner(&base, 2, 2, &gm0, 2, 2, 2.0, &mask, 2, 1, 0, 1).unwrap();
        let (info, buf) = decode_png_buf(&out);
        assert_eq!((info.width, info.height), (2, 2));
        assert_eq!(px16(&buf, 2, 0, 0, 0), 24876, "photo row unchanged");
        assert_eq!(px16(&buf, 2, 0, 1, 0), 38055, "opaque watermark → 203-nit PQ code");
        assert_eq!(px16(&buf, 2, 1, 1, 0), 38055);

        // extension: banner below the photo extends the canvas with reference white
        let out = apple_hdr_compose_inner(&base, 2, 2, &gm0, 2, 2, 2.0, &mask, 2, 1, 0, 2).unwrap();
        let (info, buf) = decode_png_buf(&out);
        assert_eq!((info.width, info.height), (2, 3));
        assert_eq!(px16(&buf, 2, 0, 1, 0), 24876, "photo row unchanged");
        assert_eq!(px16(&buf, 2, 0, 2, 0), 38055, "extension row = reference white");
        let chunks = parse_png_chunks(&out).unwrap();
        assert!(chunks.iter().any(|c| c.typ == *b"cICP" && c.data == [9, 16, 0, 1]));

        // semi-transparent watermark blends in code domain
        let mask_half = solid_rgba(2, 1, [255, 255, 255], 128);
        let out = apple_hdr_compose_inner(&base, 2, 2, &gm0, 2, 2, 2.0, &mask_half, 2, 1, 0, 1).unwrap();
        let (_, buf) = decode_png_buf(&out);
        assert_eq!(px16(&buf, 2, 0, 1, 0), 31491, "alpha=128 blend");
    }

    // ---- Ultra HDR assembly tests ----

    const GOOGLE_FIXTURE: &str = "../test_pictures/google_ultrahdr.jpg";
    const SYNTHETIC_FIXTURE: &str = "../test_pictures/synthetic-ultrahdr.jpg";

    fn jpeg_with_xmp(xmp: &str) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE1];
        let payload_len = XMP_SIG.len() + 1 + xmp.len();
        v.extend_from_slice(&((payload_len + 2) as u16).to_be_bytes());
        v.extend_from_slice(XMP_SIG);
        v.push(0);
        v.extend_from_slice(xmp.as_bytes());
        v.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02]);
        v
    }

    fn insert_com(jpeg: &mut Vec<u8>, text: &[u8]) {
        let mut seg = vec![0xFF, 0xFE];
        seg.extend_from_slice(&((text.len() + 2) as u16).to_be_bytes());
        seg.extend_from_slice(text);
        jpeg.splice(2..2, seg);
    }

    #[test]
    fn ultrahdr_gainmap_extracts_second_image() {
        let Ok(src) = std::fs::read(GOOGLE_FIXTURE) else { eprintln!("SKIP: no google fixture"); return; };
        let gm = ultrahdr_gainmap_inner(&src).unwrap();
        assert_eq!(gm.len(), 50006);
        assert_eq!(&gm[..2], &[0xFF, 0xD8]);
        assert_eq!(&gm[gm.len() - 2..], &[0xFF, 0xD9]);
        // truncation safety
        for cut in [0usize, 1, 7, 20, 100, 2000, src.len() / 2, src.len() - 1] {
            let _ = ultrahdr_gainmap_inner(&src[..cut]);
            let _ = ultrahdr_neutral_inner(&src[..cut]);
        }
    }

    #[test]
    fn ultrahdr_gainmap_stale_mpf_falls_back_to_sequential_scan() {
        const FIXTURE: &str = "../test_pictures/IMG_9678.JPG";
        let Ok(src) = std::fs::read(FIXTURE) else { eprintln!("SKIP: no stale-mpf fixture"); return; };
        // this file's MPF declares an offset 1000 bytes past the real SOI
        assert!(mpf_second_image_stale(&src), "fixture no longer exercises the stale path");
        let (abs, size) = gainmap_range(&src).unwrap();
        assert_eq!(&src[abs..abs + 2], &[0xFF, 0xD8]);
        assert_eq!(size, 108450);
        assert_eq!(abs + size, src.len());
        let gm = ultrahdr_gainmap_inner(&src).unwrap();
        assert_eq!(&gm[..2], &[0xFF, 0xD8]);
        assert_eq!(&gm[gm.len() - 2..], &[0xFF, 0xD9]);
        let info = ultrahdr_neutral_inner(&src);
        assert!(info.ok || !info.multi_channel);
    }

    fn mpf_second_image_stale(src: &[u8]) -> bool {
        for s in jpeg_segments(src) {
            if s.marker != 0xE2 {
                continue;
            }
            if let Some((off, _)) = mpf_second_image(s.payload) {
                let abs = s.start + 8 + off;
                return !(abs + 2 <= src.len() && src[abs] == 0xFF && src[abs + 1] == 0xD8);
            }
        }
        false
    }

    #[test]
    fn ultrahdr_assemble_patches_container_gainmap_length() {
        const FIXTURE: &str = "../test_pictures/IMG_9678.JPG";
        let Ok(src) = std::fs::read(FIXTURE) else { eprintln!("SKIP: no stale-mpf fixture"); return; };
        let (gm_abs, gm_size) = gainmap_range(&src).unwrap();
        let mut gm = src[gm_abs..gm_abs + gm_size].to_vec();
        // grow the gain map so a stale container length cannot coincide
        let payload = b"picseal-gm-pad";
        let mut com = vec![0xFF, 0xFE];
        com.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        com.extend_from_slice(payload);
        gm.splice(gm.len() - 2..gm.len() - 2, com);

        let out = ultrahdr_assemble_inner(&src, &src[..gm_abs], &gm).unwrap();
        let (o_abs, o_size) = gainmap_range(&out).unwrap();
        assert_eq!(o_size, gm.len());
        assert_eq!(&out[o_abs..o_abs + 2], &[0xFF, 0xD8]);
        let xmp = jpeg_segments(&out)
            .into_iter()
            .find(|s| s.marker == 0xE1 && s.payload.starts_with(XMP_SIG))
            .expect("xmp carried into base");
        let text = String::from_utf8_lossy(xmp.payload);
        let excerpt = text.find("Semantic=\"GainMap\"").map(|i| text[i..(i + 160).min(text.len())].to_string()).unwrap_or_else(|| "no GainMap item".to_string());
        assert!(text.contains(&format!("Length=\"{}\"", gm.len())), "container gain map length must match the attached image (len={}); item: {}", gm.len(), excerpt);
    }

    #[test]
    fn iso_neutral_matches_libultrahdr_metadata() {
        let Ok(src) = std::fs::read(GOOGLE_FIXTURE) else { eprintln!("SKIP: no google fixture"); return; };
        let info = ultrahdr_neutral_inner(&src);
        assert!(info.ok);
        assert!(info.multi_channel);
        assert_eq!(info.values, [171, 119, 146]);
    }

    #[test]
    fn xmp_neutral_parsing() {
        let xmp = "<x:xmpmeta><rdf:Description xmlns:hdrgm='http://ns.adobe.com/hdr-gain-map/1.0/' hdrgm:GainMapMax='2.5' hdrgm:GainMapMin='0' hdrgm:Gamma='1'/></x:xmpmeta>";
        let info = ultrahdr_neutral_inner(&jpeg_with_xmp(xmp));
        assert!(info.ok && !info.multi_channel);
        assert_eq!(info.values, [0, 0, 0]);

        let info = ultrahdr_neutral_inner(&jpeg_with_xmp("<hdrgm:GainMapMin=\"-1\" hdrgm:GainMapMax=\"1\" hdrgm:Gamma=\"1\"/>"));
        assert!(info.ok && info.values[0] == 128);

        let info = ultrahdr_neutral_inner(&jpeg_with_xmp("<hdrgm:GainMapMin=\"-1\" hdrgm:GainMapMax=\"1\" hdrgm:Gamma=\"2\"/>"));
        assert!(info.ok && info.values[0] == 64);

        let Ok(src) = std::fs::read(SYNTHETIC_FIXTURE) else { eprintln!("SKIP: no synthetic fixture"); return; };
        let info = ultrahdr_neutral_inner(&src);
        assert!(info.ok && !info.multi_channel);
        assert_eq!(info.values[0], 0);
    }

    #[test]
    fn ultrahdr_assemble_structure_roundtrip() {
        let Ok(src) = std::fs::read(GOOGLE_FIXTURE) else { eprintln!("SKIP: no google fixture"); return; };
        let (gm_abs, gm_size) = gainmap_range(&src).unwrap();
        let gm = &src[gm_abs..gm_abs + gm_size];
        let base = &src[..gm_abs];
        let old_mpf: usize = jpeg_segments(base)
            .iter()
            .filter(|s| s.marker == 0xE2 && s.payload.starts_with(b"MPF\0"))
            .map(|s| s.payload.len() + 4)
            .sum();
        assert_eq!(old_mpf, MPF_MARKER_LEN);

        let out = ultrahdr_assemble_inner(&src, base, gm).unwrap();
        assert_eq!(out.len(), base.len() - old_mpf + MPF_MARKER_LEN + gm.len());
        let info = detect_hdr_inner(&out);
        assert!(info.is_hdr && info.kind == "jpeg-gainmap");
        assert_eq!(ultrahdr_gainmap_inner(&out).unwrap(), gm.to_vec());
        let mpf = jpeg_segments(&out).into_iter().find(|s| s.marker == 0xE2 && s.payload.starts_with(b"MPF\0")).unwrap();
        let (off, size) = mpf_second_image(mpf.payload).unwrap();
        assert_eq!(size, gm.len());
        assert_eq!(mpf.start + 8 + off, out.len() - gm.len());
    }

    #[test]
    fn ultrahdr_assemble_modified_base_writes_fixture() {
        let Ok(src) = std::fs::read(GOOGLE_FIXTURE) else { eprintln!("SKIP: no google fixture"); return; };
        let (gm_abs, gm_size) = gainmap_range(&src).unwrap();
        let gm = &src[gm_abs..gm_abs + gm_size];
        let mut base = src[..gm_abs].to_vec();
        insert_com(&mut base, b"picseal ultrahdr assemble test");
        let out = ultrahdr_assemble_inner(&src, &base, gm).unwrap();
        let path = std::env::temp_dir().join("picseal_ultrahdr_assembled.jpg");
        std::fs::write(&path, &out).unwrap();
        eprintln!("WROTE {}", path.display());
        assert!(detect_hdr_inner(&out).is_hdr);
        assert_eq!(ultrahdr_gainmap_inner(&out).unwrap(), gm.to_vec());
    }

    #[test]
    fn ultrahdr_assemble_rejects_bad_input() {
        assert!(ultrahdr_assemble_inner(&[], &[], &[]).is_err());
        let jpeg = jpeg_with_xmp("<x/>");
        assert!(ultrahdr_assemble_inner(&jpeg, &jpeg, &jpeg).is_err());
    }
}
