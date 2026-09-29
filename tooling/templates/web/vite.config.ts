import { defineConfig } from 'vite-plus'
import tailwindcss from '@tailwindcss/vite'

// Sources live in resources/; the build lands in public/build/, which the Rust
// server serves (views from public/build/views, assets under /build/).
export default defineConfig({
  root: 'resources',
  base: '/build/',
  publicDir: false,
  plugins: [tailwindcss()],
  build: {
    outDir: '../public/build',
    emptyOutDir: true,
    rollupOptions: { input: 'resources/views/welcome.html' },
  },
})
