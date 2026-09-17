// Apple HDR（HEIC gain map）导出：容器解析（Rust）→ libheif 解 base + gain map
// → Rust 按 Apple 公式重建 HDR（Display P3 → BT.2020、PQ 量化）并合成水印
// → 输出带 cICP 的 16bit PQ PNG 或规范元数据的 Ultra HDR JPEG（体积约 1/20）。
import type { HeicDecodeResult } from './HeicUtils'
import type { BannerMask } from './ImageUtils'
import { apple_hdr_compose_png, apple_hdr_iso_gainmap, apple_jpeg_headroom, heic_apple_info, ultrahdr_create, ultrahdr_gainmap } from '../wasm/gen_brand_photo_pictrue'
import { decodeAppleHdrLayers } from './HeicUtils'
import { buildBannerMask, createExportCanvas, grayThumb, jpegGuessWideGamut, loadImage, orientGainMap } from './ImageUtils'
import { embedExifRaw, readExifOrientation } from './JpegExifUtils'

export interface AppleHdrInfo {
  ok: boolean
  kind: string
  gainmap_item_id: number
  headroom: number
}

export function probeAppleHdr(bytes: Uint8Array): AppleHdrInfo | null {
  try {
    const info = heic_apple_info(bytes) as unknown as AppleHdrInfo
    return info?.ok ? info : null
  }
  catch (e) {
    console.warn('Apple HDR probe failed:', e)
    return null
  }
}

function toBlobJpeg(canvas: HTMLCanvasElement, quality: number): Promise<Blob | null> {
  return new Promise(resolve => canvas.toBlob(resolve, 'image/jpeg', quality))
}

// 由已合成的 base 画布 + Apple gain map 数值（未标定的 RGBA 灰阶）生成
// Ultra HDR JPEG：gain map 经 Apple 公式转 ISO 数值（Rust），缩放/延展与 base 对齐，
// 横幅/延展区填 0（中性）；元数据由 ultrahdr_create 按 Apple/Google 规范布局从零写入
async function assembleAppleHdrJpeg(
  canvas: HTMLCanvasElement,
  mask: BannerMask,
  W: number,
  H: number,
  gmData: Uint8ClampedArray,
  gmW: number,
  gmH: number,
  headroom: number,
): Promise<Blob | null> {
  const baseBlob = await toBlobJpeg(canvas, 1.0)
  if (!baseBlob)
    return null

  const iso = apple_hdr_iso_gainmap(new Uint8Array(gmData.buffer), gmW, gmH, headroom)
  const gmSmall = document.createElement('canvas')
  gmSmall.width = gmW
  gmSmall.height = gmH
  const sctx = gmSmall.getContext('2d', { colorSpace: 'srgb' })
  if (!sctx)
    return null
  const gmImage = sctx.createImageData(gmW, gmH)
  for (let i = 0; i < iso.length; i++) {
    gmImage.data[i * 4] = iso[i]
    gmImage.data[i * 4 + 1] = iso[i]
    gmImage.data[i * 4 + 2] = iso[i]
    gmImage.data[i * 4 + 3] = 255
  }
  sctx.putImageData(gmImage, 0, 0)

  const outW = Math.max(1, Math.round(gmW * W / mask.naturalWidth))
  const outH = Math.max(1, Math.round(gmH * H / mask.naturalHeight))
  const gmCanvas = document.createElement('canvas')
  gmCanvas.width = outW
  gmCanvas.height = outH
  const gctx = gmCanvas.getContext('2d', { colorSpace: 'srgb' })
  if (!gctx)
    return null
  gctx.fillStyle = '#000'
  gctx.fillRect(0, 0, outW, outH)
  gctx.drawImage(gmSmall, 0, 0, gmW, gmH)
  const bx = Math.max(0, Math.round(mask.offX * outW / W))
  const by = Math.max(0, Math.round(mask.offY * outH / H))
  const bw = Math.min(outW - bx, Math.round(mask.width * outW / W))
  const bh = outH - by
  if (bw > 0 && bh > 0)
    gctx.fillRect(bx, by, bw, bh)
  const gmBlob = await toBlobJpeg(gmCanvas, 1.0)
  if (!gmBlob)
    return null

  const t0 = performance.now()
  const assembled = ultrahdr_create(new Uint8Array(await baseBlob.arrayBuffer()), new Uint8Array(await gmBlob.arrayBuffer()), headroom)
  console.log(`Apple HDR Ultra HDR JPEG assembled: base ${W}x${H}, gainmap ${outW}x${outH}, headroom=${headroom.toFixed(4)}, ${baseBlob.size}B + ${gmBlob.size}B -> ${assembled.length}B in ${Math.round(performance.now() - t0)}ms`)
  return new Blob([assembled], { type: 'image/jpeg' })
}

// Apple HDR → Ultra HDR JPEG：Apple 编码的 gain map 转成 ISO 21496-1 数值
// （Rust），base 与 gain map 分别在画布上合成/缩放后编码 JPEG，再由 Rust 写入
// hdrgm XMP / ISO APP2 / MPF 组装。体积约 PQ PNG 的 1/20，Apple/Android/Chrome 通用。
export async function appleHdrExportJpeg(previewDom: HTMLElement, file: File, info: AppleHdrInfo): Promise<Blob | null> {
  try {
    const mask = await buildBannerMask(previewDom)
    if (!mask)
      return null
    const layers = await decodeAppleHdrLayers(file, info.gainmap_item_id)
    if (!layers)
      return null
    if (mask.offX < 0 || mask.offX + mask.width > layers.base.width)
      return null

    const W = layers.base.width
    const H = Math.max(layers.base.height, mask.offY + mask.height)

    // base：原始分辨率 P3 画布（HEIC base 为 Display P3），横幅在照片下方时延展白底
    const canvas = createExportCanvas(W, H, true)
    const ctx = canvas.getContext('2d')
    if (!ctx)
      return null
    ctx.fillStyle = '#fff'
    ctx.fillRect(0, 0, W, H)
    const baseData = ctx.createImageData(layers.base.width, layers.base.height)
    baseData.data.set(layers.base.data)
    ctx.putImageData(baseData, 0, 0)
    const maskImg = await loadImage(mask.url)
    ctx.drawImage(maskImg, mask.offX, mask.offY, mask.width, mask.height)

    return await assembleAppleHdrJpeg(canvas, mask, W, H, layers.gainmap.data, layers.gainmap.width, layers.gainmap.height, info.headroom)
  }
  catch (e) {
    console.warn('Apple HDR JPEG export failed:', e)
    return null
  }
}

// Apple 风格 gain map JPEG（主图无 hdrgm/ISO 标记，gain map 带 Apple 私有 XMP / MakerNote，
// iOS 上传 HEIC 时的转码结果与 Photos 导出的 HDR JPEG 都是这种结构）：
// 数值口径与 Apple HDR HEIC 相同，复用同一套 Apple → ISO 重建与规范元数据组装
export async function appleJpegHdrExportJpeg(
  previewDom: HTMLElement,
  file: File,
  exifEnable: boolean,
  exifBlob: Blob | null,
): Promise<Blob | null> {
  try {
    const bytes = new Uint8Array(await file.arrayBuffer())
    const headroom = apple_jpeg_headroom(bytes)
    if (!(headroom > 1.0)) {
      console.warn('Apple JPEG HDR export skipped: no headroom')
      return null
    }
    const mask = await buildBannerMask(previewDom)
    if (!mask || mask.offX < 0 || mask.offX + mask.width > mask.naturalWidth)
      return null

    const W = mask.naturalWidth
    const H = Math.max(mask.naturalHeight, mask.offY + mask.height)

    // base：色域跟随源（P3 源保持 P3），横幅在照片下方时延展白底
    const bitmap = await createImageBitmap(file)
    const canvas = createExportCanvas(W, H, jpegGuessWideGamut(bytes))
    const ctx = canvas.getContext('2d')
    if (!ctx) {
      bitmap.close()
      return null
    }
    ctx.fillStyle = '#fff'
    ctx.fillRect(0, 0, W, H)
    ctx.drawImage(bitmap, 0, 0)
    bitmap.close()
    const maskImg = await loadImage(mask.url)
    ctx.drawImage(maskImg, mask.offX, mask.offY, mask.width, mask.height)
    // 缩略图取照片区域（不含横幅），避免横幅的白色拉高相关性
    const baseThumb = grayThumb(canvas, mask.naturalWidth, mask.naturalHeight)

    // gain map：不做色彩转换解码（imageOrientation 'none'）保证 Apple 采样值不被
    // ICC/EXIF 改写；浏览器绘制 base 时会应用其 EXIF 方向，gain map 需手动跟随
    const gainMap = ultrahdr_gainmap(bytes)
    const gmBitmap = await createImageBitmap(new Blob([new Uint8Array(gainMap)], { type: 'image/jpeg' }), { colorSpaceConversion: 'none', imageOrientation: 'none' })
    const oriented = orientGainMap(gmBitmap, mask.naturalWidth, mask.naturalHeight, baseThumb, readExifOrientation(bytes))
    gmBitmap.close()
    const gmW = oriented.width
    const gmH = oriented.height
    const gmCtx = oriented.canvas.getContext('2d', { colorSpace: 'srgb', willReadFrequently: true })
    if (!gmCtx)
      return null
    const gmData = gmCtx.getImageData(0, 0, gmW, gmH).data

    const blob = await assembleAppleHdrJpeg(canvas, mask, W, H, gmData, gmW, gmH, headroom)
    if (!blob)
      return null
    return exifEnable && exifBlob ? embedExifRaw(exifBlob, blob) : blob
  }
  catch (e) {
    console.warn('Apple JPEG HDR export failed:', e)
    return null
  }
}

export async function appleHdrExport(previewDom: HTMLElement, file: File, info: AppleHdrInfo): Promise<Blob | null> {
  try {
    const mask = await buildBannerMask(previewDom)
    if (!mask)
      return null
    const layers = await decodeAppleHdrLayers(file, info.gainmap_item_id)
    if (!layers)
      return null
    if (mask.offX < 0 || mask.offX + mask.width > layers.base.width)
      return null

    const bannerImg = await loadImage(mask.url)
    const mc = document.createElement('canvas')
    mc.width = mask.width
    mc.height = mask.height
    const mctx = mc.getContext('2d')
    if (!mctx)
      return null
    mctx.drawImage(bannerImg, 0, 0)
    const maskData = new Uint8Array(mctx.getImageData(0, 0, mask.width, mask.height).data.buffer)

    const t0 = performance.now()
    const out = apple_hdr_compose_png(
      new Uint8Array(layers.base.data.buffer),
      layers.base.width,
      layers.base.height,
      new Uint8Array(layers.gainmap.data.buffer),
      layers.gainmap.width,
      layers.gainmap.height,
      info.headroom,
      maskData,
      mask.width,
      mask.height,
      mask.offX,
      mask.offY,
    )
    console.log(`Apple HDR PNG composed: ${layers.base.width}x${layers.base.height} + banner @(${mask.offX},${mask.offY}) ${mask.width}x${mask.height}, headroom=${info.headroom.toFixed(4)}, out=${out.length}B in ${Math.round(performance.now() - t0)}ms`)
    return new Blob([out], { type: 'image/png' })
  }
  catch (e) {
    console.warn('Apple HDR export failed, falling back to SDR path:', e)
    return null
  }
}

export type { HeicDecodeResult }
