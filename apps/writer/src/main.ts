import { createApp } from 'vue'
import App from '@/App.vue'
import '@/styles/theme.css'
import '@/styles/tokens.css'
import { applyInitialShellTheme } from '@/composables/useShellTheme'

// 先把默认配色（跟随系统）写到根元素，避免偏好加载完成前闪一下浅色。
applyInitialShellTheme()

createApp(App).mount('#app')
