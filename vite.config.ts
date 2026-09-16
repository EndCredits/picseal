import react from '@vitejs/plugin-react'
import { visualizer } from 'rollup-plugin-visualizer'
import { defineConfig } from 'vite'
import { ViteImageOptimizer } from 'vite-plugin-image-optimizer'
import { VitePWA } from 'vite-plugin-pwa'
import wasm from 'vite-plugin-wasm'

export default defineConfig({
  plugins: [
    react(),
    wasm(),
    VitePWA({
      workbox: {
        // libheif（HEIC 回退解码，~1.9MB）按需加载，不进预缓存
        globIgnores: ['**/libheif/**'],
      },
    }),
    visualizer({ open: false }),
    ViteImageOptimizer({
      test: /\.(jpe?g|png|gif|tiff|webp|svg|avif)$/i,
      includePublic: true,
      logStats: true,
      ansiColors: true,
      png: {
        quality: 100,
      },
      jpeg: {
        quality: 100,
      },
      jpg: {
        quality: 100,
      },
      tiff: {
        quality: 100,
      },
      gif: {},
      webp: {
        lossless: true,
      },
      avif: {
        lossless: true,
      },
    }),
  ],
  server: {
    port: 3000,
  },
  build: {
    outDir: 'dist',
    target: 'esnext',
  },
  optimizeDeps: {
    exclude: ['picseal'],
  },
  // base: 'https://zhiweio.github.io/picseal/',
})
