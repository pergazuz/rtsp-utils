import path from 'node:path'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': path.resolve(import.meta.dirname, './src'),
    },
  },
  server: {
    port: 5173,
    // In development the UI runs on its own port, so control API calls are
    // proxied through to the Rust server. Must match DEFAULT_API_BIND in
    // src/presentation/cli.rs -- deliberately not 8080, which a neighbouring
    // service binds as a wildcard and would otherwise shadow.
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:8556',
        changeOrigin: true,
      },
    },
  },
  build: {
    // Where `rtsp-utils --api` looks for the built UI.
    outDir: 'dist',
    emptyOutDir: true,
  },
})
