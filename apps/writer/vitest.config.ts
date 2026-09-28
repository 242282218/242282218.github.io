import { defineConfig } from 'vitest/config'
import vue from '@vitejs/plugin-vue'
import { fileURLToPath, URL } from 'node:url'

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
  },
  test: {
    environment: 'jsdom',
    include: ['src/tests/**/*.test.ts'],
    // 补齐 jsdom 缺失的浏览器 API（Range 测量等），见 setup.ts 的说明。
    setupFiles: ['src/tests/setup.ts'],
    testTimeout: 30000,
  },
})
