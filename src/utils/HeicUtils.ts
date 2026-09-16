// 非 WebKit 浏览器（Chrome/Firefox）的 HEIC 解码回退：
// 按需动态 import libheif-js 预打包 ESM（wasm 内联，约 1.9MB，Vite 打成懒加载 chunk），
// 解码为 SDR RGBA → JPEG Blob 供预览/导出。HDR 不保留（HEVC 重编码不可行）。

export interface HeicDecodeResult {
  blob: Blob
  width: number
  height: number
  ms: number
}

export async function decodeHeicToJpeg(file: Blob, quality = 0.95): Promise<HeicDecodeResult | null> {
  try {
    const t0 = performance.now()
    const { default: createLibHeif } = await import('libheif-js/libheif-wasm/libheif-bundle.mjs')
    const libheif = await createLibHeif()
    const decoder = new libheif.HeifDecoder()
    const images = decoder.decode(new Uint8Array(await file.arrayBuffer()))
    if (!images.length)
      throw new Error('no image in HEIC')
    const image = images[0]
    try {
      const width = image.get_width()
      const height = image.get_height()
      if (!width || !height)
        throw new Error('bad HEIC geometry')
      const canvas = document.createElement('canvas')
      canvas.width = width
      canvas.height = height
      const ctx = canvas.getContext('2d')
      if (!ctx)
        throw new Error('no 2d context')
      const imageData = ctx.createImageData(width, height)
      await new Promise<void>((resolve, reject) => {
        image.display(imageData, data => (data ? resolve() : reject(new Error('libheif display failed'))))
      })
      ctx.putImageData(imageData, 0, 0)
      const blob = await new Promise<Blob | null>(resolve => canvas.toBlob(resolve, 'image/jpeg', quality))
      if (!blob)
        throw new Error('JPEG encode failed')
      const ms = Math.round(performance.now() - t0)
      console.log(`HEIC WASM decode: ${width}x${height}, ${(file.size / 1048576).toFixed(1)}MB input -> ${(blob.size / 1048576).toFixed(1)}MB JPEG in ${ms}ms`)
      return { blob, width, height, ms }
    }
    finally {
      image.free()
    }
  }
  catch (e) {
    console.warn('HEIC WASM decode failed:', e)
    return null
  }
}
