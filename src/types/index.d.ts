declare module '*.wasm' {
  const content: any
  export default content
}

// libheif-js 预打包浏览器版（wasm 内联）：默认导出是 emscripten 模块工厂
declare module 'libheif-js/libheif-wasm/libheif-bundle.mjs' {
  export interface HeifImage {
    get_width: () => number
    get_height: () => number
    display: (
      target: { data: Uint8ClampedArray, width: number, height: number },
      callback: (data: unknown | null) => void,
    ) => void
    free: () => void
  }

  const createLibHeif: (overrides?: Record<string, unknown>) => Promise<{
    HeifDecoder: new () => { decode: (data: Uint8Array) => HeifImage[] }
  }>
  export default createLibHeif
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
