import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'

import App from '@/App'
import { Toaster } from '@/components/ui/sonner'
import '@/index.css'

/** Follows the operating system's light/dark preference. */
function applyTheme(dark: boolean) {
  document.documentElement.classList.toggle('dark', dark)
}

const prefersDark = window.matchMedia('(prefers-color-scheme: dark)')
applyTheme(prefersDark.matches)
prefersDark.addEventListener('change', (e) => applyTheme(e.matches))

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
    <Toaster position="bottom-right" richColors />
  </StrictMode>,
)
