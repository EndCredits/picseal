// Apple HDR（HEIC gain map）导出：容器解析（Rust）→ libheif 解 base + gain map
// → Rust 按 Apple 公式重建 HDR（Display P3 → BT.2020、PQ 量化）并合成水印
// → 输出带 cICP 的 16bit PQ PNG。
import type { HeicDecodeResult } from './HeicUtils'
import { apple_hdr_compose_png, heic_apple_info } from '../wasm/gen_brand_photo_pictrue'
import { decodeAppleHdrLayers } from './HeicUtils'
import { buildBannerMask, loadImage } from './ImageUtils'

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
