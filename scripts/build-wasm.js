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

console.log('WASM build completed!')
