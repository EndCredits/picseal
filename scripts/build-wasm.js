import { execSync } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const __filename = fileURLToPath(import.meta.url)
const __dirname = path.dirname(__filename)

// 构建 WASM
console.log('Building WASM...')
execSync('wasm-pack build src-wasm --target web --out-dir ../src/wasm', {
  stdio: 'inherit',
})

// 确保 WASM 目录存在
const wasmDir = path.join(__dirname, '../src/wasm')
if (!fs.existsSync(wasmDir)) {
  fs.mkdirSync(wasmDir, { recursive: true })
}

// HEIC 解码回退（非 WebKit 浏览器）：把 libheif-js 预打包 ESM（wasm 内联）复制到
// public/，运行时按需动态 import，避免 Vite/TLA 插件处理超大 chunk 与 PWA 预缓存
console.log('Copying libheif bundle...')
const heicDir = path.join(__dirname, '../public/libheif')
fs.mkdirSync(heicDir, { recursive: true })
fs.copyFileSync(
  path.join(__dirname, '../node_modules/libheif-js/libheif-wasm/libheif-bundle.mjs'),
  path.join(heicDir, 'libheif-bundle.js'),
)

console.log('WASM build completed!')
