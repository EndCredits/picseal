// Apple HDR（HEIC gain map）导出：容器解析（Rust）→ libheif 解 base + gain map
// → Rust 按 Apple 公式重建 HDR（Display P3 → BT.2020、PQ 量化）并合成水印
// → 输出带 cICP 的 16bit PQ PNG。体积大但保真度最高。
import type { HeicDecodeResult } from './HeicUtils'
import { apple_hdr_compose_png, apple_hdr_iso_gainmap, heic_apple_info, ultrahdr_create } from '../wasm/gen_brand_photo_pictrue'
import { decodeAppleHdrLayers } from './HeicUtils'
import { buildBannerMask, createExportCanvas, loadImage } from './ImageUtils'

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
    const baseBlob = await toBlobJpeg(canvas, 1.0)
    if (!baseBlob)
      return null

    // gain map：Apple → ISO 数值（Rust），中性值 0；缩放/延展与 Ultra HDR 路径一致
    const iso = apple_hdr_iso_gainmap(new Uint8Array(layers.gainmap.data.buffer), layers.gainmap.width, layers.gainmap.height, info.headroom)
    const gmSmall = document.createElement('canvas')
    gmSmall.width = layers.gainmap.width
    gmSmall.height = layers.gainmap.height
    const sctx = gmSmall.getContext('2d', { colorSpace: 'srgb' })
    if (!sctx)
      return null
    const gmImage = sctx.createImageData(layers.gainmap.width, layers.gainmap.height)
    for (let i = 0; i < iso.length; i++) {
      gmImage.data[i * 4] = iso[i]
      gmImage.data[i * 4 + 1] = iso[i]
      gmImage.data[i * 4 + 2] = iso[i]
      gmImage.data[i * 4 + 3] = 255
    }
    sctx.putImageData(gmImage, 0, 0)

    const outW = Math.max(1, Math.round(layers.gainmap.width * W / mask.naturalWidth))
    const outH = Math.max(1, Math.round(layers.gainmap.height * H / mask.naturalHeight))
    const gmCanvas = document.createElement('canvas')
    gmCanvas.width = outW
    gmCanvas.height = outH
    const gctx = gmCanvas.getContext('2d', { colorSpace: 'srgb' })
    if (!gctx)
      return null
    gctx.fillStyle = '#000'
    gctx.fillRect(0, 0, outW, outH)
    gctx.drawImage(gmSmall, 0, 0, layers.gainmap.width, layers.gainmap.height)
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
    const assembled = ultrahdr_create(new Uint8Array(await baseBlob.arrayBuffer()), new Uint8Array(await gmBlob.arrayBuffer()), info.headroom)
    console.log(`Apple HDR Ultra HDR JPEG assembled: base ${W}x${H}, gainmap ${outW}x${outH}, headroom=${info.headroom.toFixed(4)}, ${baseBlob.size}B + ${gmBlob.size}B -> ${assembled.length}B in ${Math.round(performance.now() - t0)}ms`)
    return new Blob([assembled], { type: 'image/jpeg' })
  }
  catch (e) {
    console.warn('Apple HDR JPEG export failed:', e)
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
