declare module '*.wasm' {
  const content: any
  export default content
}

interface ExifData {
  tag: string
  value: string
  value_with_unit: string
}

interface HdrInfo {
  is_hdr: boolean
  kind: string
}

export interface ExifParamsForm {
  model: string
  date: string
  gps: string
  device: string
  brand: string
  brand_url: string
  scale: number
  fontSize: string
  fontWeight: string
  fontFamily: string
}
