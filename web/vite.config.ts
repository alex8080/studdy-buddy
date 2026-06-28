import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import type { IncomingMessage } from 'node:http'

function bypassHtml(req: IncomingMessage) {
  if (req.headers.accept?.includes('text/html')) return '/index.html'
}

export default defineConfig({
  plugins: [react()],
  envPrefix: 'STUDYBUDDY_',
  server: {
    proxy: {
      '^/(health|ingest|cards|reviews)': { target: 'http://localhost:8080', bypass: bypassHtml },
    },
  },
})
