// 非 WebKit 浏览器（Chrome/Firefox）的 HEIC 解码回退：
// 按需动态 import libheif-js 预打包 ESM（wasm 内联，约 1.9MB，Vite 打成懒加载 chunk），
// 解码为 SDR RGBA → JPEG Blob 供预览/导出。HDR 不保留（HEVC 重编码不可行）。

export interface HeicDecodeResult {
  blob: Blob
  width: number
  height: number
  ms: number
}

export interface RgbaImage {
  data: Uint8ClampedArray
  width: number
  height: number
}

let libheifPromise: Promise<any> | null = null

function loadLibheif(): Promise<any> {
  if (!libheifPromise) {
    libheifPromise = (async () => {
      const { default: createLibHeif } = await import('libheif-js/libheif-wasm/libheif-bundle.mjs')
      return await createLibHeif()
    })()
  }
  return libheifPromise
}

async function decodeHandleToRgba(libheif: any, handle: any): Promise<RgbaImage> {
  const res = await libheif.heif_js_decode_image2(handle, libheif.heif_colorspace.heif_colorspace_RGB, libheif.heif_chroma.heif_chroma_interleaved_RGBA)
  if (!res || res.code)
    throw new Error(`decode failed: ${res && res.message}`)
  const ch = res.channels.find((c: any) => c.id === libheif.heif_channel.heif_channel_interleaved) ?? res.channels[0]
  const out = new Uint8ClampedArray(ch.width * ch.height * 4)
  if (ch.stride === ch.width * 4) {
    out.set(ch.data.subarray(0, out.length))
  }
  else {
    for (let y = 0; y < ch.height; y++)
      out.set(ch.data.subarray(y * ch.stride, y * ch.stride + ch.width * 4), y * ch.width * 4)
  }
  libheif.heif_image_release(res.image)
  return { data: out, width: ch.width, height: ch.height }
}

// 解出 Apple HDR HEIC 的 base（SDR，Display P3）与 gain map（灰度，item id 由 Rust 容器解析给出）
export async function decodeAppleHdrLayers(file: Blob, gainmapItemId: number): Promise<{ base: RgbaImage, gainmap: RgbaImage, ms: number } | null> {
  try {
    const t0 = performance.now()
    const libheif = await loadLibheif()
    const bytes = new Uint8Array(await file.arrayBuffer())
    const ctx = libheif.heif_context_alloc()
    try {
      const rc = libheif.heif_context_read_from_memory(ctx, bytes)
      if (rc && rc.code !== libheif.heif_error_code.heif_error_Ok)
        throw new Error(`container read failed: ${rc.message}`)
      const ids = libheif.heif_js_context_get_list_of_top_level_image_IDs(ctx)
      const primary = libheif.heif_js_context_get_image_handle(ctx, ids[0])
      if (!primary || primary.code)
        throw new Error('primary image handle missing')
      const base = await decodeHandleToRgba(libheif, primary)
      const gmHandle = libheif.heif_js_context_get_image_handle(ctx, gainmapItemId)
      if (!gmHandle || gmHandle.code)
        throw new Error(`gain map handle missing (item ${gainmapItemId})`)
      const gainmap = await decodeHandleToRgba(libheif, gmHandle)
      const ms = Math.round(performance.now() - t0)
      console.log(`Apple HDR layers: base ${base.width}x${base.height}, gainmap ${gainmap.width}x${gainmap.height} in ${ms}ms`)
      return { base, gainmap, ms }
    }
    finally {
      libheif.heif_context_free(ctx)
    }
  }
  catch (e) {
    console.warn('Apple HDR layer decode failed:', e)
    return null
  }
}

export async function decodeHeicToJpeg(file: Blob, quality = 0.95): Promise<HeicDecodeResult | null> {
  try {
    const t0 = performance.now()
    const libheif = await loadLibheif()
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
