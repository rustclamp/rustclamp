import { defineConfig } from 'vite-plus'
import { globSync } from 'node:fs'
import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'

// Sources live in app/resources/; the build lands in public/build/, which the Rust
// server serves (views from public/build/views, assets under /build/).
export default defineConfig({
  root: 'app/resources',
  base: '/build/',
  publicDir: false,
  plugins: [tailwindcss(), react()],
  build: {
    outDir: '../../public/build',
    emptyOutDir: true,
    // Every page in app/resources/views/ is built, errors/404.html overrides included.
    rollupOptions: { input: globSync('app/resources/views/**/*.html') },
  },
})
