import type { RcFile } from 'antd/es/upload'

import type { ExifParamsForm } from '../types'
import { message } from 'antd'
import { useRef, useState } from 'react'
import { getBrandUrl } from '../utils/BrandUtils'
import { compositePngExport, dataURLtoBlob, getRandomImage, parseExifData, rasterizeDomToDataUrl } from '../utils/ImageUtils'
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

        const blobUrl = URL.createObjectURL(file)
        if (!await canDecode(blobUrl)) {
          URL.revokeObjectURL(blobUrl)
          if (hdrInfo?.is_hdr)
            message.warning('当前浏览器无法解码该 HDR 图片，请使用 Safari 打开，或先转换为 SDR 的 JPEG/PNG 再上传', 300)
          else if (/heic|heif/i.test(file.type))
            message.warning('当前浏览器不支持 HEIC，请使用 Safari 打开，或先转换为 JPEG/PNG 再上传', 300)
          else
            message.warning('当前浏览器无法解码该图片格式，请换一张照片', 300)
          return
        }

        const gainMapJpeg = !!hdrInfo?.is_hdr && hdrInfo.kind === 'jpeg-gainmap'
        setHdrGainMapJpeg(gainMapJpeg)
        if (gainMapJpeg)
          message.info('检测到 HDR（gain map JPEG）：导出将保留 HDR，水印区域按 SDR 白处理', 300)
        else if (hdrInfo?.is_hdr)
          message.warning('检测到 HDR 照片：导出后将丢失 HDR 信息，输出为 SDR', 300)

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
        setUploadImgType(file.type)
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
        message.error('无法识别照片特定数据，请换一张照片', 300)
      }
    }
    load()
    return false
  }

  // 导出图片
  const handleDownload = async (exifEnable: boolean): Promise<void> => {
    const previewDom = document.getElementById('preview')
    const zoomRatio = 4

    try {
      let downloadBlob: Blob | null = null
      let dataUrl = ''

      // gain map JPEG：WASM Ultra HDR 组装（原生分辨率水印 + 保留 HDR）
      if ((uploadImgType === 'image/jpeg' || uploadImgType === 'image/jpg') && hdrGainMapJpeg && uploadFile) {
        console.log('wasm ultrahdr composite export')
        downloadBlob = await compositeUltraHdrExport(previewDom, uploadFile, exifEnable, exifBlob)
      }

      // PNG 输入优先走 WASM 高保真管线（原分辨率/位深/元数据保留）
      if (!downloadBlob && uploadImgType === 'image/png' && uploadFile) {
        console.log('wasm png composite export')
        downloadBlob = await compositePngExport(previewDom, uploadFile)
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
      // 扩展名跟随实际输出格式（PNG 输入输出 PNG，其余输出 JPEG）
      const fileExt = uploadImgType === 'image/png' ? 'png' : 'jpg'
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
      default: 'var(--font-family-default)',
      caveat: 'var(--font-family-caveat)',
      misans: 'var(--font-family-misans)',
      helvetica: 'var(--font-family-helvetica)',
      futura: 'var(--font-family-futura)',
      avenir: 'var(--font-family-avenir)',
      didot: 'var(--font-family-didot)',
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
