import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import { rmSync } from 'node:fs';
import { resolve } from 'node:path';

/** macOS 自带苹方（运行时 @font-face 的 local() 命中系统字体），
    打包时剔除 webfont 以减小体积；其他平台照常携带。
    可用 VITE_STRIP_PINGFANG=1 在任意平台强制剔除（也便于验证） */
function stripPingFangOnDarwin(): Plugin {
  let outDir = '';
  return {
    name: 'strip-pingfang-on-darwin',
    apply: 'build',
    configResolved(cfg) {
      outDir = resolve(cfg.root, cfg.build.outDir);
    },
    closeBundle() {
      const strip = process.env.VITE_STRIP_PINGFANG === '1' || process.platform === 'darwin';
      if (!strip) return;
      rmSync(resolve(outDir, 'fonts'), { recursive: true, force: true });
      console.log('[strip-pingfang-on-darwin] removed fonts/ from', outDir);
    },
  };
}

export default defineConfig({
  plugins: [
    react(),
    stripPingFangOnDarwin(),
  ],
  resolve: {
    alias: {
      '@': resolve(__dirname, 'src'),
    },
  },
  server: {
    port: 5173,
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