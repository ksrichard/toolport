import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// In dev, the backend runs on the host. In Docker, Caddy does this routing.
const backend = { target: 'http://localhost:8080', changeOrigin: true }

export default defineConfig({
  plugins: [react()],
  server: {
    proxy: { '/api': backend, '/auth': backend, '/mcp': backend },
  },
})
