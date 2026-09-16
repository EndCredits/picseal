import type { BannerMask } from './ImageUtils'
import { ultrahdr_assemble, ultrahdr_gainmap, ultrahdr_neutral } from '../wasm/gen_brand_photo_pictrue'
import { buildBannerMask, createExportCanvas, jpegGuessWideGamut, loadImage } from './ImageUtils'
import { embedExifRaw } from './JpegExifUtils'

interface NeutralInfo {
  ok: boolean
  multi_channel: boolean
  values: number[]
}

function toBlobJpeg(canvas: HTMLCanvasElement, quality: number): Promise<Blob | null> {
  return new Promise(resolve => canvas.toBlob(resolve, 'image/jpeg', quality))
}

// 横幅区域 gain map 置中性增益（log2 boost 0 → HDR 重构下维持 SDR 白 ≈203nit）；
// 横幅在照片下方时延展 gain map 画布（中性填充），保证照片区 HDR 对齐；
// 解码不做色彩转换，保证 gain map 采样值不被 ICC 改写
async function neutralizeGainMap(gainMap: Uint8Array, mask: BannerMask, baseW: number, baseH: number, neutral: NeutralInfo): Promise<Uint8Array> {
  const bitmap = await createImageBitmap(new Blob([gainMap], { type: 'image/jpeg' }), { colorSpaceConversion: 'none' })
  const gw = bitmap.width
  const gh = bitmap.height
  if (!gw || !gh) {
    bitmap.close()
    return gainMap
  }
  const outW = Math.max(1, Math.round(gw * baseW / mask.naturalWidth))
  const outH = Math.max(1, Math.round(gh * baseH / mask.naturalHeight))
  const canvas = document.createElement('canvas')
  canvas.width = outW
  canvas.height = outH
  const ctx = canvas.getContext('2d', { colorSpace: 'srgb', willReadFrequently: true })
  if (!ctx) {
    bitmap.close()
    return gainMap
  }
  const [r, g, b] = neutral.values
  ctx.fillStyle = `rgb(${r},${g},${b})`
  ctx.fillRect(0, 0, outW, outH)
  ctx.drawImage(bitmap, 0, 0, gw, gh)
  bitmap.close()
  const bx = Math.max(0, Math.round(mask.offX * outW / baseW))
  const by = Math.max(0, Math.round(mask.offY * outH / baseH))
  const bw = Math.min(outW - bx, Math.round(mask.width * outW / baseW))
  const bh = outH - by
  if (bw > 0 && bh > 0)
    ctx.fillRect(bx, by, bw, bh)
  const blob = await toBlobJpeg(canvas, 0.95)
  if (!blob)
    return gainMap
  return new Uint8Array(await blob.arrayBuffer())
}

// Ultra HDR（gain map JPEG）导出：原生分辨率水印 base（横幅在照片下方时延展白底画布）
// + gain map 横幅区中性化 + MPF/元数据重组
export async function compositeUltraHdrExport(
  previewDom: HTMLElement,
  file: File,
  exifEnable: boolean,
  exifBlob: Blob | null,
): Promise<Blob | null> {
  try {
    const mask = await buildBannerMask(previewDom)
    // 横幅超出照片宽度的布局不支持（当前设计不会出现）
    if (!mask || mask.offX < 0 || mask.offX + mask.width > mask.naturalWidth)
      return null

    const W = mask.naturalWidth
    const H = Math.max(mask.naturalHeight, mask.offY + mask.height)
    const original = new Uint8Array(await file.arrayBuffer())
    const neutral = ultrahdr_neutral(original) as unknown as NeutralInfo
    const gainMap = ultrahdr_gainmap(original)

    const bitmap = await createImageBitmap(file)
    // base 画布色域跟随源文件（P3 源保持 P3，sRGB 源不再升 P3），
    // 保证逐通道 gain map 在原色彩空间内应用
    const wide = jpegGuessWideGamut(original)
    const canvas = createExportCanvas(W, H, wide)
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
    let baseBlob = await toBlobJpeg(canvas, 0.95)
    if (!baseBlob)
      return null
    if (exifEnable && exifBlob)
      baseBlob = embedExifRaw(exifBlob, baseBlob)

    const gainMapFinal = neutral?.ok
      ? await neutralizeGainMap(gainMap, mask, W, H, neutral)
      : gainMap

    const assembled = ultrahdr_assemble(original, new Uint8Array(await baseBlob.arrayBuffer()), gainMapFinal)
    console.log('WASM Ultra HDR assembled:', `base ${W}x${H}, banner @(${mask.offX},${mask.offY}) ${mask.width}x${mask.height}, neutral=${JSON.stringify(neutral)}, out=${assembled.length}B`)
    return new Blob([assembled], { type: 'image/jpeg' })
  }
  catch (e) {
    console.warn('Ultra HDR export failed, falling back to SDR canvas path:', e)
    return null
  }
}
