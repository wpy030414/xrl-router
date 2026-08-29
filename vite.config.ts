import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { resolve } from 'node:path';

export default defineConfig({
  plugins: [
    react(),
  ],
  resolve: {
    alias: {
      '@': resolve(__dirname, 'src'),
    },
  },
  server: {
    port: 5173,
    watch: {
      // Rust 编译产物（.pdb 等）会被 rustc 独占锁定，Vite 的 FSWatcher 尝试 watch 时触发 EBUSY。
      // target/ 与前端无关，直接排除喵～
      ignored: ['**/src-tauri/target/**'],
    },
    proxy: {
      '/api': 'http://127.0.0.1:19068',
      '/v1': 'http://127.0.0.1:19068',
      '/health': 'http://127.0.0.1:19068',
      '/install/download': 'http://127.0.0.1:19068',
      '/install/info': 'http://127.0.0.1:19068',
      '/ws': {
        target: 'ws://127.0.0.1:19068',
        ws: true,
      },
    },
  },
  build: {
    outDir: 'dist',
    assetsDir: 'assets',
  },
});