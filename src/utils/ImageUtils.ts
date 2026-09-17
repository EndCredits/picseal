import type { ExifData, ExifParamsForm } from '../types'
import domtoimage from 'dom-to-image'
import moment from 'moment'
import { composite_png } from '../wasm/gen_brand_photo_pictrue'
import { BrandsList } from './BrandUtils'
import { embedExifRaw } from './JpegExifUtils'

export const DefaultPictureExif = {
  model: 'XIAOMI 13 ULTRA',
  date: moment().format('YYYY.MM.DD HH:mm'),
  gps: `41°12'47"N 124°00'16"W`,
  device: '75mm f/1.8 1/33s ISO800',
  brand: 'leica',
  brand_url: './brand/leica.svg',
  scale: 0.8,
  fontSize: 'normal',
  fontWeight: 'bold',
  fontFamily: 'misans',
}

export const ExhibitionImages = [
  './exhibition/apple.jpg',
  './exhibition/canon.jpg',
  './exhibition/dji.jpg',
  './exhibition/fujifilm.jpg',
  './exhibition/huawei.jpg',
  './exhibition/leica.jpg',
  './exhibition/xiaomi.jpg',
  './exhibition/nikon.jpg',
  './exhibition/sony.jpg',
  './exhibition/panasonic.jpg',
]

// 格式化 GPS 数据
export function formatGPS(gps: string | undefined, gpsRef: string | undefined): string {
  if (!gps)
    return ''
  const [degrees, minutes, seconds, dir] = gps
    .match(/(\d+\.?\d*)|([NSWE]$)/gim)
    .map(item => (!Number.isNaN(Number(item)) ? `${~~item}`.padStart(2, '0') : item))
  if (gpsRef)
    return `${degrees}°${minutes}'${seconds}"${gpsRef}`
  else if (dir)
    return `${degrees}°${minutes}'${seconds}"${dir}`
  else return `${degrees}°${minutes}'${seconds}"`
}

// 格式化品牌
export function formatBrand(make: string | undefined): string {
  if ((make || '') === 'Arashi Vision') {
    return 'insta360'
  }
  const brand = (make || '').toLowerCase()
  for (const b of BrandsList.map(b => b.toLowerCase())) {
    if (brand.includes(b)) {
      return b
    }
  }
  return brand
}

// 格式化曝光时间
export function formatExposureTime(exposureTime: string | undefined): string {
  if (!exposureTime)
    return ''
  const [numerator, denominator] = exposureTime.split('/').filter(Boolean).map(item => Math.floor(Number(item)))
  return [numerator, denominator].join('/')
}

// 格式化拍摄时间
export function formatDateTimeOriginal(dateTimeOriginal: string | undefined): string {
  if (!dateTimeOriginal)
    return moment().format('YYYY.MM.DD HH:mm')
  return moment(dateTimeOriginal).format('YYYY-MM-DD HH:mm')
}

export function formatModel(model: string, brand: string): string {
  const camera_model: string = model.replace(/[",]/g, '')
  if (brand === 'sony') {
    return camera_model.replace(/[",]/g, '').replace('ILCE-', 'α').toLowerCase()
  }
  if (brand === 'nikon corporation') {
    return camera_model.replace(/Z/gi, 'ℤ')
  }
  if (brand === 'panasonic') {
    if (camera_model.startsWith('DMC-') || camera_model.startsWith('DC-'))
      return `LUMIX ${camera_model}`
  }
  return camera_model
}

// 解析 EXIF 数据
export function parseExifData(data: ExifData[]): Partial<ExifParamsForm> {
  const exifValues = new Map(data.map(item => [item.tag, item.value]))
  const exifValuesWithUnit = new Map(data.map(item => [item.tag, item.value_with_unit]))
  const make: string = (exifValues.get('Make') || '').replace(/[",]/g, '')
  const brand: string = formatBrand(make || 'unknow')
  if (brand === 'unknow') {
    return DefaultPictureExif
  }

  const exif = {
    GPSLatitude: '',
    GPSLatitudeRef: '',
    GPSLongitude: '',
    GPSLongitudeRef: '',
    FocalLengthIn35mmFilm: '',
    FocalLength: '',
    FNumber: '',
    ExposureTime: '',
    PhotographicSensitivity: '',
    Model: '',
    Make: '',
    DateTimeOriginal: '',
  }
  exif.Make = make
  exif.Model = `${formatModel((exifValues.get('Model') || ''), brand)}`
  exif.GPSLatitude = exifValues.get('GPSLatitude') || ''
  exif.GPSLatitudeRef = exifValues.get('GPSLatitudeRef') || ''
  exif.GPSLongitude = exifValues.get('GPSLongitude') || ''
  exif.GPSLongitudeRef = exifValues.get('GPSLongitudeRef') || ''
  exif.FocalLengthIn35mmFilm = exifValuesWithUnit.get('FocalLengthIn35mmFilm') || ''
  exif.FocalLength = exifValuesWithUnit.get('FocalLength') || ''
  exif.FNumber = exifValuesWithUnit.get('FNumber') || ''
  exif.ExposureTime = exifValues.get('ExposureTime') || ''
  exif.PhotographicSensitivity = exifValues.get('PhotographicSensitivity') || ''
  exif.DateTimeOriginal = exifValues.get('DateTimeOriginal') || ''

  const gps = `${formatGPS(exif.GPSLatitude, exif.GPSLatitudeRef)} ${formatGPS(exif.GPSLongitude, exif.GPSLongitudeRef)}`
  const device = [
    `${(exif.FocalLengthIn35mmFilm || exif.FocalLength).replace(/\s+/g, '')}`,
    exif.FNumber?.split('/')?.map((n, i) => (i ? (+n).toFixed(1) : n)).join('/'),
    exif.ExposureTime ? `${formatExposureTime(exif.ExposureTime)}s` : '',
    exif.PhotographicSensitivity ? `ISO${exif.PhotographicSensitivity}` : '',
  ]
    .filter(Boolean)
    .join(' ')
  return {
    model: exif.Model || 'PICSEAL',
    date: `${formatDateTimeOriginal(exif.DateTimeOriginal)}`,
    gps,
    device,
    brand,
  }
}

// 在组件初始化时随机选择一张照片
export function getRandomImage() {
  const randomIndex = Math.floor(Math.random() * ExhibitionImages.length)
  return ExhibitionImages[randomIndex]
}

export function dataURLtoBlob(dataURL: string): Blob {
  const byteString: string = atob(dataURL.split(',')[1])
  const mimeString: string = dataURL.split(',')[0].split(':')[1].split(';')[0]
  const ab = new ArrayBuffer(byteString.length)
  const ia = new Uint8Array(ab)
  for (let i: number = 0; i < byteString.length; i++) {
    ia[i] = byteString.charCodeAt(i)
  }
  return new Blob([ab], { type: mimeString })
}

export interface RasterizeOptions {
  format: 'jpeg' | 'png'
  width: number
  height: number
  quality?: number
  style?: Partial<CSSStyleDeclaration>
}

// 创建导出画布：wide 时优先 Display P3 广色域，不支持或非广色域源时回退 sRGB（如 Firefox）
export function createExportCanvas(width: number, height: number, wide = true): HTMLCanvasElement {
  let canvas: HTMLCanvasElement | null = null
  if (wide) {
    try {
      const c = document.createElement('canvas')
      c.width = width
      c.height = height
      const ctx = c.getContext('2d', { colorSpace: 'display-p3' })
      if (ctx && ctx.getContextAttributes()?.colorSpace === 'display-p3')
        canvas = c
    }
    catch {
      canvas = null
    }
  }
  if (!canvas) {
    canvas = document.createElement('canvas')
    canvas.width = width
    canvas.height = height
  }
  return canvas
}

export function loadImage(uri: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const image = new Image()
    image.onload = () => resolve(image)
    image.onerror = reject
    image.src = uri
  })
}

function delay(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms))
}

// 将 DOM 节点光栅化为 dataURL，等效替换 dom-to-image 的 toPng/toJpeg，
// 区别是画布使用 Display P3（保留广色域，导出文件自动携带 P3 ICC）
export async function rasterizeDomToDataUrl(node: HTMLElement, options: RasterizeOptions): Promise<string> {
  const svgUrl = await domtoimage.toSvg(node, options)
  const image = await loadImage(svgUrl)
  await delay(100)
  const canvas = createExportCanvas(options.width, options.height)
  const ctx = canvas.getContext('2d')
  if (!ctx)
    throw new Error('Failed to get 2d context')
  ctx.drawImage(image, 0, 0)
  if (options.format === 'png')
    return canvas.toDataURL()
  return canvas.toDataURL('image/jpeg', options.quality ?? 1.0)
}

// 从 PNG chunk 猜测是否广色域（iCCP 名称 / cICP 原色 / sRGB chunk）
export function pngGuessWideGamut(bytes: Uint8Array): boolean {
  if (bytes.length < 8)
    return false
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  let off = 8
  let sawIccp = false
  while (off + 12 <= bytes.length) {
    const len = dv.getUint32(off)
    const type = String.fromCharCode(bytes[off + 4], bytes[off + 5], bytes[off + 6], bytes[off + 7])
    if (type === 'IDAT' || type === 'IEND')
      break
    const body = bytes.subarray(off + 8, off + 8 + len)
    if (type === 'iCCP') {
      sawIccp = true
      const nameEnd = body.indexOf(0)
      const name = new TextDecoder().decode(body.subarray(0, nameEnd < 0 ? 80 : nameEnd))
      if (/p3|2020|adobe|prophoto/i.test(name))
        return true
    }
    else if (type === 'cICP' && len >= 1 && (body[0] === 9 || body[0] === 12)) {
      return true
    }
    else if (type === 'sRGB') {
      return false
    }
    off += 12 + len
  }
  return sawIccp // 未知名称的 iCCP 按广色域处理（与 P3 预览一致）
}

// 横幅 DOM → 原生分辨率 PNG mask；几何与预览 DOM 完全一致（PNG/JPEG 导出共用）
export interface BannerMask {
  url: string
  width: number
  height: number
  offX: number
  offY: number
  naturalWidth: number
  naturalHeight: number
}

export async function buildBannerMask(previewDom: HTMLElement): Promise<BannerMask | null> {
  const img = previewDom.querySelector('.preview-picture') as HTMLImageElement | null
  const banner = previewDom.querySelector('.preview-info') as HTMLElement | null
  if (!img || !banner || !img.naturalWidth || !img.naturalHeight)
    return null
  // 预览图切换有 0.6s 缩放过渡；过渡中 getBoundingClientRect 含 transform，几何会失真
  if (Math.abs(img.getBoundingClientRect().width - img.clientWidth) > 0.5)
    await delay(700)
  const imgRect = img.getBoundingClientRect()
  const bannerRect = banner.getBoundingClientRect()
  if (!imgRect.width || !bannerRect.width || !bannerRect.height)
    return null
  const scale = img.naturalWidth / imgRect.width
  const width = Math.max(1, Math.round(bannerRect.width * scale))
  const height = Math.max(1, Math.round(bannerRect.height * scale))
  const offX = Math.round((bannerRect.left - imgRect.left) * scale)
  const offY = Math.round((bannerRect.top - imgRect.top) * scale)
  if (offX < 0 || offY < 0 || offX + width > img.naturalWidth)
    return null
  // 整幅预览按原生分辨率光栅化后裁剪 banner 区域：banner 元素单独光栅化在部分环境
  // （含 headless Chrome）会得到全透明结果，改走与 SDR 导出同源的全幅渲染更稳
  const fullW = Math.max(1, Math.round(previewDom.clientWidth * scale))
  const fullH = Math.max(1, Math.round(previewDom.clientHeight * scale))
  const fullUrl = await rasterizeDomToDataUrl(previewDom, {
    format: 'png',
    width: fullW,
    height: fullH,
    style: { transform: `scale(${scale})`, transformOrigin: 'top left' },
  })
  const fullImg = await loadImage(fullUrl)
  // 浏览器因 canvas 面积上限静默缩小画布（移动端）时 mask 会错位，交由调用方回退
  if (fullImg.naturalWidth !== fullW || fullImg.naturalHeight !== fullH)
    return null
  const canvas = document.createElement('canvas')
  canvas.width = width
  canvas.height = height
  const ctx = canvas.getContext('2d')
  if (!ctx)
    return null
  ctx.drawImage(fullImg, offX, offY, width, height, 0, 0, width, height)
  const url = canvas.toDataURL('image/png')
  return { url, width, height, offX, offY, naturalWidth: img.naturalWidth, naturalHeight: img.naturalHeight }
}

// 从 JPEG APP2 ICC 段猜测是否广色域（与 pngGuessWideGamut 同款启发式；
// 未知命名的 ICC 按广色域处理）
export function jpegGuessWideGamut(bytes: Uint8Array): boolean {
  if (bytes.length < 4 || bytes[0] !== 0xFF || bytes[1] !== 0xD8)
    return false
  let off = 2
  let sawIcc = false
  while (off + 4 <= bytes.length) {
    if (bytes[off] !== 0xFF)
      break
    const marker = bytes[off + 1]
    if (marker === 0xDA || marker === 0xD9)
      break
    const len = (bytes[off + 2] << 8) | bytes[off + 3]
    if (len < 2 || off + 2 + len > bytes.length)
      break
    if (marker === 0xE2) {
      const payload = bytes.subarray(off + 4, off + 2 + len)
      if (payload.length > 14 && payload[0] === 0x49 && payload[1] === 0x43 && payload[2] === 0x43) {
        sawIcc = true
        const head = payload.subarray(0, Math.min(payload.length, 2048))
        // profile 描述在 ICC v2 里是 ASCII、v4 里是 UTF-16BE，两种都扫一遍
        const texts = [new TextDecoder('latin1').decode(head), new TextDecoder('utf-16be').decode(head)]
        if (texts.some(t => /p3|2020|adobe|prophoto/i.test(t)))
          return true
        // 显式标注 sRGB 的 profile（如 "sRGB Gamut with sRGB Transfer"）判为非广色域；
        // 顺序：P3 profile 的描述文本里也含 sRGB 字样，必须先判广色域关键词
        if (texts.some(t => /srgb|iec61966/i.test(t)))
          return false
      }
    }
    off += 2 + len
  }
  return sawIcc
}

// 探测 PNG 的 HDR 传输（cICP transfer 16=PQ / 18=HLG；无 cICP 时以 mDCv/cLLi 静态 HDR 度量按 PQ 处理）
export function pngHdrTransfer(bytes: Uint8Array): 'pq' | 'hlg' | null {
  if (bytes.length < 8)
    return null
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  let off = 8
  let sawCicp = false
  let cicpTransfer = 0
  let sawStaticHdr = false
  while (off + 12 <= bytes.length) {
    const len = dv.getUint32(off)
    const type = String.fromCharCode(bytes[off + 4], bytes[off + 5], bytes[off + 6], bytes[off + 7])
    if (type === 'IDAT' || type === 'IEND')
      break
    const body = bytes.subarray(off + 8, off + 8 + len)
    if (type === 'cICP' && len >= 2) {
      sawCicp = true
      cicpTransfer = body[1]
      break
    }
    if (type === 'mDCv' || type === 'cLLi')
      sawStaticHdr = true
    off += 12 + len
  }
  if (sawCicp)
    return cicpTransfer === 16 ? 'pq' : cicpTransfer === 18 ? 'hlg' : null
  return sawStaticHdr ? 'pq' : null
}

// 按 EXIF Orientation（1..8）把位图绘到新画布；1 或未知值直接返回原方向。
// 用于 gain map 与 base 的方向对齐：浏览器绘制 base 时会应用其 EXIF 方向，
// 而 gain map（第二图/aux item）通常不带方向信息，必须手动跟随
export function drawExifOriented(src: ImageBitmap, orientation: number): HTMLCanvasElement {
  const sw = src.width
  const sh = src.height
  const swap = orientation > 4
  const canvas = document.createElement('canvas')
  canvas.width = swap ? sh : sw
  canvas.height = swap ? sw : sh
  const ctx = canvas.getContext('2d', { colorSpace: 'srgb', willReadFrequently: true })
  if (!ctx)
    throw new Error('Failed to get 2d context')
  // 变换矩阵取自 blueimp/JavaScript-Load-Image 的成熟实现
  switch (orientation) {
    case 2:
      ctx.transform(-1, 0, 0, 1, sw, 0)
      break
    case 3:
      ctx.transform(-1, 0, 0, -1, sw, sh)
      break
    case 4:
      ctx.transform(1, 0, 0, -1, 0, sh)
      break
    case 5:
      ctx.transform(0, 1, 1, 0, 0, 0)
      break
    case 6:
      ctx.transform(0, 1, -1, 0, sh, 0)
      break
    case 7:
      ctx.transform(0, -1, -1, 0, sh, sw)
      break
    case 8:
      ctx.transform(0, -1, 1, 0, 0, sw)
      break
    default:
      break
  }
  ctx.drawImage(src, 0, 0)
  return canvas
}

// 粗网格灰度缩略（用于方向判定的相关性比较）：取 source 左上角 sw×sh 区域，
// 即 base 画布的照片区（横幅不计入，避免白色区域干扰相关性）
export function grayThumb(source: CanvasImageSource, sw: number, sh: number, n = 24): Float32Array {
  const canvas = document.createElement('canvas')
  canvas.width = n
  canvas.height = n
  const ctx = canvas.getContext('2d', { colorSpace: 'srgb', willReadFrequently: true })
  if (!ctx)
    return new Float32Array(0)
  ctx.drawImage(source, 0, 0, sw, sh, 0, 0, n, n)
  const d = ctx.getImageData(0, 0, n, n).data
  const out = new Float32Array(n * n)
  for (let i = 0; i < out.length; i++)
    out[i] = (d[i * 4] * 0.299 + d[i * 4 + 1] * 0.587 + d[i * 4 + 2] * 0.114) / 255
  return out
}

function grayCorrelation(a: Float32Array, b: Float32Array): number {
  if (!a.length || a.length !== b.length)
    return -2
  let ma = 0
  let mb = 0
  for (let i = 0; i < a.length; i++) {
    ma += a[i]
    mb += b[i]
  }
  ma /= a.length
  mb /= b.length
  let num = 0
  let da = 0
  let db = 0
  for (let i = 0; i < a.length; i++) {
    const x = a[i] - ma
    const y = b[i] - mb
    num += x * y
    da += x * x
    db += y * y
  }
  return da > 0 && db > 0 ? num / Math.sqrt(da * db) : -2
}

export interface OrientedGainMap {
  canvas: HTMLCanvasElement
  width: number
  height: number
}

// gain map 方向校正：先按 base 的 EXIF 方向（JPEG 输入）旋正；若宽高比仍与 base
// 转置（libheif 未对 aux 图应用 irot、或其他来源缺少方向信息），则用 gain map 与
// base 亮度的相关性在 90°/270° 中判向（gain map 高值区通常对应 base 高光）
export function orientGainMap(gm: ImageBitmap, baseW: number, baseH: number, baseThumb: Float32Array | null, exifOrientation = 1): OrientedGainMap {
  let canvas = drawExifOriented(gm, exifOrientation)
  let width = canvas.width
  let height = canvas.height
  const a = width / height
  const b = baseW / baseH
  // 容差 20%：横幅延展/历史导出文件的轻微形变会让宽高比偏离几到十几个百分点
  const transposed = Math.abs(a - 1 / b) / (1 / b) < 0.2 && Math.abs(a - b) / b > 0.2
  if (transposed && baseThumb?.length) {
    const c90 = drawExifOriented(gm, 6)
    const c270 = drawExifOriented(gm, 8)
    const r90 = grayCorrelation(baseThumb, grayThumb(c90, c90.width, c90.height))
    const r270 = grayCorrelation(baseThumb, grayThumb(c270, c270.width, c270.height))
    console.log(`gain map orientation: base ${baseW}x${baseH} vs gain map ${width}x${height} transposed, correlation 90°=${r90.toFixed(3)} 270°=${r270.toFixed(3)}`)
    if (r90 >= r270) {
      canvas = c90
    }
    else {
      canvas = c270
    }
    width = canvas.width
    height = canvas.height
  }
  return { canvas, width, height }
}

// 从 RGBA 像素（Apple HDR HEIC 的 libheif 输出）取粗网格灰度缩略
export function grayThumbFromRgba(data: Uint8ClampedArray, w: number, h: number, n = 24): Float32Array {
  const out = new Float32Array(n * n)
  if (!w || !h)
    return out
  for (let y = 0; y < n; y++) {
    for (let x = 0; x < n; x++) {
      const sx = Math.min(w - 1, Math.floor((x + 0.5) * w / n))
      const sy = Math.min(h - 1, Math.floor((y + 0.5) * h / n))
      const i = (sy * w + sx) * 4
      out[y * n + x] = (data[i] * 0.299 + data[i + 1] * 0.587 + data[i + 2] * 0.114) / 255
    }
  }
  return out
}

// PNG 高保真导出：WASM 原分辨率合成，保留位深与全部元数据 chunk；
// 不适用（palette/APNG）或失败时返回 null，由调用方回退 canvas 路径
export async function compositePngExport(previewDom: HTMLElement, file: File): Promise<Blob | null> {
  try {
    const mask = await buildBannerMask(previewDom)
    if (!mask)
      return null
    const { width: maskW, height: maskH, offX, offY, naturalWidth, naturalHeight } = mask
    const bytes = new Uint8Array(await file.arrayBuffer())

    const bannerImg = await loadImage(mask.url)
    const wide = pngGuessWideGamut(bytes)
    const hdr = pngHdrTransfer(bytes)
    const mc = document.createElement('canvas')
    mc.width = maskW
    mc.height = maskH
    // HDR 源：mask 以 sRGB 渲染，WASM 侧做 sRGB→目标原色（BT.2020/P3）转换后再编码 PQ/HLG
    const mctx = wide && !hdr
      ? mc.getContext('2d', { colorSpace: 'display-p3' })
      : mc.getContext('2d')
    if (!mctx)
      return null
    mctx.drawImage(bannerImg, 0, 0)
    const maskData = mctx.getImageData(0, 0, maskW, maskH).data

    const out = composite_png(bytes, new Uint8Array(maskData.buffer), maskW, maskH, offX, offY)
    console.log('WASM PNG composite done:', `${naturalWidth}x${naturalHeight} -> out ${out.length}B, mask ${maskW}x${maskH}@(${offX},${offY}), wide=${wide}, hdr=${hdr}`)
    return new Blob([out], { type: 'image/png' })
  }
  catch (e) {
    console.warn('WASM PNG composite failed, falling back to canvas path:', e)
    return null
  }
}

// 原生分辨率 canvas 导出（JPEG/WebP 等浏览器可解码输入）：以 1:1 绘制原图 +
// 原生分辨率 banner mask（buildBannerMask），色域跟随源（JPEG 按 ICC 启发式判定
// P3/sRGB）；超出 canvas 面积/边长上限或浏览器静默缩小画布时返回 null，由调用方
// 回退预览截图路径
const NATIVE_EXPORT_MAX_PIXELS = 64 * 1024 * 1024
const NATIVE_EXPORT_MAX_SIDE = 16384

export async function compositeNativeExport(
  previewDom: HTMLElement,
  file: File,
  exifEnable: boolean,
  exifBlob: Blob | null,
): Promise<Blob | null> {
  try {
    const img = previewDom.querySelector('.preview-picture') as HTMLImageElement | null
    if (!img || !img.complete || !img.naturalWidth || !img.naturalHeight)
      return null
    const mask = await buildBannerMask(previewDom)
    if (!mask)
      return null

    const W = img.naturalWidth
    const H = Math.max(img.naturalHeight, mask.offY + mask.height)
    if (W * H > NATIVE_EXPORT_MAX_PIXELS || W > NATIVE_EXPORT_MAX_SIDE || H > NATIVE_EXPORT_MAX_SIDE) {
      console.log('native resolution export skipped: canvas limit', `${W}x${H}`)
      return null
    }
    let wide = true
    if (/jpe?g/i.test(file.type) || /\.jpe?g$/i.test(file.name)) {
      try {
        wide = jpegGuessWideGamut(new Uint8Array(await file.arrayBuffer()))
      }
      catch {
        wide = true
      }
    }
    const canvas = createExportCanvas(W, H, wide)
    // 浏览器因面积/尺寸上限静默缩小画布（移动端）时回退
    if (canvas.width !== W || canvas.height !== H) {
      console.log('native resolution export skipped: canvas clamped', `${canvas.width}x${canvas.height}`)
      return null
    }
    const ctx = canvas.getContext('2d')
    if (!ctx)
      return null
    ctx.fillStyle = '#fff'
    ctx.fillRect(0, 0, W, H)
    ctx.drawImage(img, 0, 0)
    const bannerImg = await loadImage(mask.url)
    ctx.drawImage(bannerImg, mask.offX, mask.offY, mask.width, mask.height)
    const blob = await new Promise<Blob | null>(resolve => canvas.toBlob(resolve, 'image/jpeg', 1.0))
    if (!blob)
      return null
    console.log('native resolution export done:', `${W}x${H}, wide=${wide}, ${blob.size}B`)
    return exifEnable && exifBlob ? embedExifRaw(exifBlob, blob) : blob
  }
  catch (e) {
    console.warn('native resolution export failed, falling back to preview screenshot:', e)
    return null
  }
}
