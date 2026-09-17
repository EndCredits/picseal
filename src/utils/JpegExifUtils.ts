const SOS = 0xFFDA
const APP1 = 0xFFE1
const EXIF = 0x45786966
const JPEG = 0xFFD8 // JPEG start marker
const ORIENTATION_TAG = 0x0112
const TIFF_OFFSET = 10 // FF E1 len(2) + "Exif\0\0"(6)

// 导出像素已按 EXIF 方向校正（浏览器渲染 <img> 时应用方向），回嵌的 APP1 需把
// IFD0 的 Orientation 归一化为 1，否则遵循 EXIF 的查看器会二次旋转（竖拍方向错误）
function normalizeExifOrientation(app1: Uint8Array): void {
  if (app1.length < TIFF_OFFSET + 8 || app1[0] !== 0xFF || app1[1] !== 0xE1)
    return
  try {
    const little = app1[TIFF_OFFSET] === 0x49 && app1[TIFF_OFFSET + 1] === 0x49
    const big = app1[TIFF_OFFSET] === 0x4D && app1[TIFF_OFFSET + 1] === 0x4D
    if (!little && !big)
      return
    const dv = new DataView(app1.buffer, app1.byteOffset, app1.byteLength)
    const ifd = TIFF_OFFSET + dv.getUint32(TIFF_OFFSET + 4, little)
    if (ifd + 2 > app1.length)
      return
    const count = dv.getUint16(ifd, little)
    for (let i = 0; i < count; i++) {
      const entry = ifd + 2 + i * 12
      if (entry + 12 > app1.length)
        return
      if (dv.getUint16(entry, little) === ORIENTATION_TAG && dv.getUint16(entry + 2, little) === 3) {
        // SHORT 类型：值内联在 4 字节 value 字段的前 2 字节，按字节序写 1
        app1[entry + 8] = little ? 1 : 0
        app1[entry + 9] = little ? 0 : 1
        return
      }
    }
  }
  catch {
    // 结构异常时保持原样，交给查看器处理
  }
}

export function extractExifRaw(raw: Blob): Promise<Blob> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onloadend = async (e) => {
      const buffer = e.target?.result
      if (!(buffer instanceof ArrayBuffer))
        return reject(new Error('Failed to read raw image data'))

      const view = new DataView(buffer)
      let offset = 0
      if (view.getUint16(offset) !== JPEG)
        return reject(new Error('not a valid jpeg'))
      offset += 2

      while (offset + 4 <= view.byteLength) {
        const marker = view.getUint16(offset)
        if (marker === SOS)
          break
        const size = view.getUint16(offset + 2)
        if (size < 2)
          break
        if (marker === APP1 && view.getUint32(offset + 4) === EXIF) {
          if (offset + 2 + size > view.byteLength)
            break
          const app1 = new Uint8Array(buffer, offset, 2 + size).slice()
          normalizeExifOrientation(app1)
          return resolve(new Blob([app1]))
        }
        offset += 2 + size
      }
      return resolve(new Blob())
    }
    reader.readAsArrayBuffer(raw)
  })
}

export function embedExifRaw(exifRaw: Blob, targetImg: Blob): Blob {
  return new Blob([targetImg.slice(0, 2), exifRaw, targetImg.slice(2)], {
    type: 'image/jpeg',
  })
}
