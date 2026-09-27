import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  // Tauri 的 devUrl 是固定端口，端口被占用时应直接失败而不是悄悄换端口。
  server: {
    port: 1420,
    strictPort: true,
  },
  // Tauri 需要固定的产物目录。
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'chrome110',
    sourcemap: false,
  },
})
