import type { BannerMask } from './ImageUtils'
import { ultrahdr_assemble, ultrahdr_gainmap, ultrahdr_neutral } from '../wasm/gen_brand_photo_pictrue'
import { buildBannerMask, createExportCanvas, loadImage } from './ImageUtils'
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
// 解码不做色彩转换，保证 gain map 采样值不被 ICC 改写
async function neutralizeGainMap(gainMap: Uint8Array, mask: BannerMask, neutral: NeutralInfo): Promise<Uint8Array> {
  const bitmap = await createImageBitmap(new Blob([gainMap], { type: 'image/jpeg' }), { colorSpaceConversion: 'none' })
  const gw = bitmap.width
  const gh = bitmap.height
  if (!gw || !gh) {
    bitmap.close()
    return gainMap
  }
  const gx = Math.max(0, Math.round(mask.offX * gw / mask.naturalWidth))
  const gy = Math.max(0, Math.round(mask.offY * gh / mask.naturalHeight))
  const width = Math.min(gw - gx, Math.round(mask.width * gw / mask.naturalWidth))
  const height = Math.min(gh - gy, Math.round(mask.height * gh / mask.naturalHeight))
  if (width <= 0 || height <= 0) {
    bitmap.close()
    return gainMap
  }
  const canvas = document.createElement('canvas')
  canvas.width = gw
  canvas.height = gh
  const ctx = canvas.getContext('2d', { colorSpace: 'srgb', willReadFrequently: true })
  if (!ctx) {
    bitmap.close()
    return gainMap
  }
  ctx.drawImage(bitmap, 0, 0)
  bitmap.close()
  const data = ctx.getImageData(gx, gy, width, height)
  const [r, g, b] = neutral.values
  for (let i = 0; i < data.data.length; i += 4) {
    data.data[i] = r
    data.data[i + 1] = g
    data.data[i + 2] = b
    data.data[i + 3] = 255
  }
  ctx.putImageData(data, gx, gy)
  const blob = await toBlobJpeg(canvas, 0.95)
  if (!blob)
    return gainMap
  return new Uint8Array(await blob.arrayBuffer())
}

// Ultra HDR（gain map JPEG）导出：原生分辨率水印 base + gain map 横幅区中性化 + MPF/元数据重组
export async function compositeUltraHdrExport(
  previewDom: HTMLElement,
  file: File,
  exifEnable: boolean,
  exifBlob: Blob | null,
): Promise<Blob | null> {
  try {
    const mask = await buildBannerMask(previewDom)
    // 画布向下延展的场景不支持（gain map 无法覆盖新增区域）
    if (!mask || mask.offY + mask.height > mask.naturalHeight)
      return null

    const original = new Uint8Array(await file.arrayBuffer())
    const neutral = ultrahdr_neutral(original) as unknown as NeutralInfo
    const gainMap = ultrahdr_gainmap(original)

    const bitmap = await createImageBitmap(file)
    const canvas = createExportCanvas(mask.naturalWidth, mask.naturalHeight)
    const ctx = canvas.getContext('2d')
    if (!ctx) {
      bitmap.close()
      return null
    }
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
      ? await neutralizeGainMap(gainMap, mask, neutral)
      : gainMap

    const assembled = ultrahdr_assemble(original, new Uint8Array(await baseBlob.arrayBuffer()), gainMapFinal)
    console.log('WASM Ultra HDR assembled:', `${mask.naturalWidth}x${mask.naturalHeight}, neutral=${JSON.stringify(neutral)}, out=${assembled.length}B`)
    return new Blob([assembled], { type: 'image/jpeg' })
  }
  catch (e) {
    console.warn('Ultra HDR export failed, falling back to SDR canvas path:', e)
    return null
  }
}
