import type { RcFile } from 'antd/es/upload'

import type { ExifParamsForm } from '../types'
import type { AppleHdrInfo } from '../utils/AppleHdrUtils'
import type { HeicDecodeResult } from '../utils/HeicUtils'
import { message } from 'antd'
import { useRef, useState } from 'react'
import { appleHdrExport, appleHdrExportJpeg, appleJpegHdrExportJpeg, probeAppleHdr } from '../utils/AppleHdrUtils'
import { getBrandUrl } from '../utils/BrandUtils'
import { decodeHeicToJpeg } from '../utils/HeicUtils'
import { compositeNativeExport, compositePngExport, dataURLtoBlob, getRandomImage, parseExifData, rasterizeDomToDataUrl } from '../utils/ImageUtils'
import { embedExifRaw, extractExifRaw } from '../utils/JpegExifUtils'
import { compositeUltraHdrExport } from '../utils/UltraHdrUtils'
import { detect_hdr, get_exif } from '../wasm/gen_brand_photo_pictrue'

export function useImageHandlers(formRef: any, initialFormValue: ExifParamsForm) {
  const [formValue, setFormValue] = useState<ExifParamsForm>(initialFormValue)
  const [imgUrl, setImgUrl] = useState<string>(getRandomImage())
  const imgRef = useRef<HTMLImageElement>(null)
  const [uploadImgType, setUploadImgType] = useState<string>()
  const [uploadFile, setUploadFile] = useState<RcFile | null>(null)
  const [exifBlob, setExifBlob] = useState<Blob | null>(null)
  const [hdrGainMapJpeg, setHdrGainMapJpeg] = useState(false)
  // Apple 风格 gain map JPEG（iOS 上传 HEIC 的转码结构）：导出走 Apple JPEG HDR 路径
  const [appleJpegGainMap, setAppleJpegGainMap] = useState(false)
  const [appleHdr, setAppleHdr] = useState<AppleHdrInfo | null>(null)
  // Apple HDR 导出格式：Ultra HDR JPEG（默认，体积小）或 16bit PQ PNG（保真）
  const [hdrFormat, setHdrFormat] = useState<'ultrahdr' | 'png'>('ultrahdr')

  // 探测浏览器能否解码该图片（Chrome/Firefox 不支持 HEIC）
  async function canDecode(blobUrl: string): Promise<boolean> {
    try {
      const probe = new Image()
      probe.src = blobUrl
      await probe.decode()
      return true
    }
    catch {
      return false
    }
  }

  // 处理文件上传
  const handleAdd = (file: RcFile): false => {
    const load = async () => {
      try {
        const bytes = new Uint8Array(await file.arrayBuffer())
        const hdrInfo = detect_hdr(bytes) as HdrInfo
        if (hdrInfo?.is_hdr)
          console.log('HDR input detected: ', hdrInfo.kind)

        let blobUrl = URL.createObjectURL(file)
        let previewType = file.type
        let wasmDecodedHeic: HeicDecodeResult | null = null
        const isHeic = /heic|heif/i.test(file.type) || /\.(?:heic|heif)$/i.test(file.name)
        const appleInfo = isHeic ? probeAppleHdr(bytes) : null
        setAppleHdr(appleInfo)
        if (!await canDecode(blobUrl)) {
          // 非 WebKit 浏览器：懒加载 libheif wasm 解 HEIC（SDR），保留预览/导出可用
          wasmDecodedHeic = isHeic ? await decodeHeicToJpeg(file) : null
          if (!wasmDecodedHeic) {
            URL.revokeObjectURL(blobUrl)
            if (hdrInfo?.is_hdr)
              message.warning('当前浏览器无法解码该 HDR 图片，请使用 Safari 打开，或先转换为 SDR 的 JPEG/PNG 再上传', 5)
            else if (isHeic)
              message.warning('HEIC 解码失败，请使用 Safari 打开，或先转换为 JPEG/PNG 再上传', 5)
            else
              message.warning('当前浏览器无法解码该图片格式，请换一张照片', 5)
            return
          }
          URL.revokeObjectURL(blobUrl)
          blobUrl = URL.createObjectURL(wasmDecodedHeic.blob)
          previewType = 'image/jpeg'
        }

        const gainMapJpeg = !!hdrInfo?.is_hdr && hdrInfo.kind === 'jpeg-gainmap'
        // Apple 风格 gain map JPEG（主图无 hdrgm/ISO 标记）：iOS 上传 HEIC 时的转码结果即此结构
        const appleJpegGainMap = !!hdrInfo?.is_hdr && hdrInfo.kind === 'apple-gainmap'
        const hdrPng = !!hdrInfo?.is_hdr && file.type === 'image/png' && hdrInfo.kind.startsWith('png-')
        setHdrGainMapJpeg(gainMapJpeg)
        setAppleJpegGainMap(appleJpegGainMap)
        if (wasmDecodedHeic)
          message.info(`当前浏览器不支持 HEIC，已用内置解码器转为 SDR 预览（${wasmDecodedHeic.width}×${wasmDecodedHeic.height}，${wasmDecodedHeic.ms}ms）${appleInfo ? '；导出可重建 HDR（Ultra HDR JPEG / 16bit PQ PNG 可选）' : hdrInfo?.is_hdr ? '；导出将丢失 HDR' : ''}`, 5)
        else if (appleInfo)
          message.info(`检测到 Apple HDR HEIC：导出可重建 HDR，默认 Ultra HDR JPEG，可选 16bit PQ PNG（headroom ${appleInfo.headroom.toFixed(2)}×）`, 5)
        else if (gainMapJpeg)
          message.info('检测到 HDR（gain map JPEG）：导出将保留 HDR，水印区域按 SDR 白处理', 5)
        else if (appleJpegGainMap)
          message.info('检测到 Apple 风格 HDR JPEG（gain map 在第二图，iOS 上传/相册导出的结构）：导出将保留 HDR，水印区域按 SDR 白处理', 5)
        else if (hdrPng && hdrInfo.kind !== 'png-hlg')
          message.info('检测到 HDR PNG（PQ）：导出保留 HDR，水印按 203nit / 目标原色映射', 5)
        else if (hdrPng)
          message.info('检测到 HDR PNG（HLG）：导出保留 HDR，水印按 BT.2408 参考白（75% 信号）映射', 5)
        else if (hdrInfo?.is_hdr)
          message.warning('检测到 HDR 照片：导出后将丢失 HDR 信息，输出为 SDR', 5)

        const exifData = get_exif(bytes)
        const parsedExif = parseExifData(exifData)
        const updatedFormValue = {
          ...formValue,
          ...parsedExif,
          brand_url: getBrandUrl(parsedExif.brand),
        }
        console.log('original EXIF data: ', exifData)
        console.log('parsed EXIF data: ', parsedExif)
        formRef.current.setFieldsValue(updatedFormValue)
        setFormValue(updatedFormValue)
        setImgUrl(blobUrl)
        setUploadImgType(previewType)
        setUploadFile(file)
        try {
          setExifBlob(await extractExifRaw(new Blob([file])))
        }
        catch {
          // 非 JPEG 输入没有可回嵌的 APP1 段，忽略
          setExifBlob(null)
        }
      }
      catch (error) {
        console.error('Error parsing EXIF data:', error)
        message.error('无法识别照片特定数据，请换一张照片', 5)
      }
    }
    load()
    return false
  }

  // 导出图片
  const handleDownload = async (exifEnable: boolean): Promise<void> => {
    const previewDom = document.getElementById('preview')
    if (!previewDom) {
      message.error('导出失败，请重试')
      return
    }
    const zoomRatio = 4

    try {
      let downloadBlob: Blob | null = null
      let dataUrl = ''

      // Apple HDR HEIC：默认输出 Ultra HDR JPEG（体积小、生态通用），可选 16bit PQ PNG
      if (appleHdr && uploadFile) {
        if (hdrFormat === 'ultrahdr') {
          console.log('wasm apple hdr ultra hdr jpeg export')
          downloadBlob = await appleHdrExportJpeg(previewDom, uploadFile, appleHdr)
        }
        if (!downloadBlob) {
          console.log('wasm apple hdr composite export')
          downloadBlob = await appleHdrExport(previewDom, uploadFile, appleHdr)
        }
      }

      // gain map JPEG：WASM Ultra HDR 组装（原生分辨率水印 + 保留 HDR）
      if (!downloadBlob && (uploadImgType === 'image/jpeg' || uploadImgType === 'image/jpg') && hdrGainMapJpeg && uploadFile) {
        console.log('wasm ultrahdr composite export')
        downloadBlob = await compositeUltraHdrExport(previewDom, uploadFile, exifEnable, exifBlob)
      }

      // Apple 风格 gain map JPEG：Apple 数值 → ISO 数值 + 规范元数据重建（同 Apple HDR HEIC 路径）
      if (!downloadBlob && (uploadImgType === 'image/jpeg' || uploadImgType === 'image/jpg') && appleJpegGainMap && uploadFile) {
        console.log('wasm apple jpeg hdr export')
        downloadBlob = await appleJpegHdrExportJpeg(previewDom, uploadFile, exifEnable, exifBlob)
      }

      // PNG 输入优先走 WASM 高保真管线（原分辨率/位深/元数据保留）
      if (!downloadBlob && uploadImgType === 'image/png' && uploadFile) {
        console.log('wasm png composite export')
        downloadBlob = await compositePngExport(previewDom, uploadFile)
      }

      // 其余非 PNG 输入：原生分辨率 canvas 导出（1:1 原图 + 原生 banner），
      // 超出 canvas 上限或失败时回退预览截图路径
      if (!downloadBlob && uploadImgType !== 'image/png' && uploadFile) {
        console.log('native resolution composite export')
        downloadBlob = await compositeNativeExport(previewDom, uploadFile, exifEnable, exifBlob)
      }

      if (!downloadBlob) {
        // canvas 路径：JPEG 导出，或 PNG 的回退
        const rasterOptions = {
          width: previewDom.clientWidth * zoomRatio,
          height: previewDom.clientHeight * zoomRatio,
          style: { transform: `scale(${zoomRatio})`, transformOrigin: 'top left' },
        }
        if (uploadImgType === 'image/png') {
          console.log('dom to png')
          dataUrl = await rasterizeDomToDataUrl(previewDom, { ...rasterOptions, format: 'png' })
        }
        else {
          dataUrl = await rasterizeDomToDataUrl(previewDom, { ...rasterOptions, format: 'jpeg', quality: 1.0 })
        }
        if (exifEnable && exifBlob) {
          if (uploadImgType === 'image/jpeg' || uploadImgType === 'image/jpg') {
            console.log('embed exif in jpg')
            downloadBlob = await embedExifRaw(exifBlob, dataURLtoBlob(dataUrl))
          }
          else {
            console.warn('EXIF blob data can only be embedded in JPEG or JPG images.')
          }
        }
      }

      const link = document.createElement('a')
      link.href = downloadBlob ? URL.createObjectURL(downloadBlob) : dataUrl
      // 扩展名跟随实际输出格式（blob 优先，其次 PNG 输入，其余 JPEG）
      const fileExt = downloadBlob?.type?.includes('png') || uploadImgType === 'image/png' ? 'png' : 'jpg'
      link.download = `${Date.now()}.${fileExt}`
      document.body.appendChild(link)
      link.click()
      link.remove()
    }
    catch (error) {
      console.error('Download Error:', error)
      message.error('导出失败，请重试')
    }
  }

  // 处理表单更新
  const handleFormChange = (_: any, values: ExifParamsForm): void => {
    setFormValue({
      ...values,
      brand_url: getBrandUrl(values.brand),
    })
  }

  const handleScaleChange = (scale) => {
    document.documentElement.style.setProperty('--banner-scale', scale)
    setFormValue(prev => ({ ...prev, scale }))
  }

  const handleFontSizeChange = (fontSize) => {
    const sizeMap = {
      small: 'var(--font-size-small)',
      normal: 'var(--font-size-normal)',
      large: 'var(--font-size-large)',
    }
    document.documentElement.style.setProperty('--current-font-size', sizeMap[fontSize])
    setFormValue(prev => ({ ...prev, fontSize }))
  }

  const handleFontWeightChange = (fontWeight) => {
    const weightMap = {
      normal: 'var(--font-weight-normal)',
      bold: 'var(--font-weight-bold)',
      black: 'var(--font-weight-black)',
    }
    document.documentElement.style.setProperty('--current-font-weight', weightMap[fontWeight])
    setFormValue(prev => ({ ...prev, fontWeight }))
  }

  const handleFontFamilyChange = (fontFamily) => {
    const familyMap = {
      'default': 'var(--font-family-default)',
      'caveat': 'var(--font-family-caveat)',
      'misans': 'var(--font-family-misans)',
      'google-sans-flex': 'var(--font-family-google-sans-flex)',
      'helvetica': 'var(--font-family-helvetica)',
      'futura': 'var(--font-family-futura)',
      'avenir': 'var(--font-family-avenir)',
      'didot': 'var(--font-family-didot)',
    }
    document.documentElement.style.setProperty('--current-font-family', familyMap[fontFamily])
    setFormValue(prev => ({ ...prev, fontFamily }))
  }

  // 更新处理展览按钮点击的函数
  const handleExhibitionClick = async (brand: string) => {
    const brandImageUrl = `./exhibition/${brand.toLowerCase()}.jpg`

    // Add fade class to trigger animation
    if (imgRef.current) {
      imgRef.current.classList.add('fade')
    }

    // Wait for the fade effect to complete before changing the image
    setTimeout(async () => {
      setImgUrl(brandImageUrl)

      // Read image file and parse EXIF data
      const response = await fetch(brandImageUrl)
      const blob = await response.blob()
      const arrayBuffer = await blob.arrayBuffer()
      const exifData = get_exif(new Uint8Array(arrayBuffer))
      const parsedExif = parseExifData(exifData)

      const updatedFormValue = {
        ...formValue,
        ...parsedExif,
        brand_url: getBrandUrl(parsedExif.brand),
      }

      formRef.current.setFieldsValue(updatedFormValue)
      setFormValue(updatedFormValue)

      // Remove fade class after the new image is set
      if (imgRef.current) {
        imgRef.current.classList.remove('fade')
        imgRef.current.classList.add('loaded') // Ensure the loaded class is added
      }
    }, 500) // Match the duration of the CSS transition
  }

  return {
    appleHdr,
    hdrFormat,
    setHdrFormat,
    imgRef,
    imgUrl,
    setImgUrl,
    formValue,
    setFormValue,
    handleAdd,
    handleDownload,
    handleFormChange,
    handleFontSizeChange,
    handleFontWeightChange,
    handleFontFamilyChange,
    handleScaleChange,
    handleExhibitionClick,
  }
}
