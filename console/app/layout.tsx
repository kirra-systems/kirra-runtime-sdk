import type { Metadata } from 'next'
import localFont from 'next/font/local'
import './globals.css'
import { Sidebar } from '@/components/shell/sidebar'
import { TopNav } from '@/components/shell/top-nav'

// Self-hosted variable fonts — the exact files kirrasystems.com serves
// (copied from website/fonts/). No build-time network fetch, so the console
// builds air-gapped and is guaranteed pixel-identical to the brand faces.
const display = localFont({
  src: './fonts/space-grotesk-var-latin.woff2',
  weight: '300 700',
  variable: '--font-display',
})
const sans = localFont({
  src: './fonts/inter-var-latin.woff2',
  weight: '100 900',
  variable: '--font-sans',
})
const mono = localFont({
  src: './fonts/jetbrains-mono-var-latin.woff2',
  weight: '100 800',
  variable: '--font-mono',
})

export const metadata: Metadata = {
  title: 'Kirra Mission Console',
  description: 'Fleet operations & AI safety governance for autonomous systems.',
}

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html
      lang="en"
      className={`${display.variable} ${sans.variable} ${mono.variable} dark`}
      suppressHydrationWarning
    >
      <body className="font-sans antialiased">
        {/* Theme init before paint. Operators default to dark regardless of OS
            preference (control-room convention); an explicit choice persists. */}
        <script
          dangerouslySetInnerHTML={{
            __html: `try{if(localStorage.getItem('kirra-console-theme')==='light')document.documentElement.classList.add('light')}catch(e){}`,
          }}
        />
        <a
          href="#console-main"
          className="sr-only focus:not-sr-only focus:fixed focus:left-4 focus:top-4 focus:z-50 focus:rounded-lg focus:bg-ice focus:px-4 focus:py-2 focus:font-medium focus:text-bg"
        >
          Skip to content
        </a>
        <div className="app-bg" aria-hidden="true" />
        <div className="relative z-10 flex h-screen overflow-hidden">
          <Sidebar />
          <div className="flex min-w-0 flex-1 flex-col">
            <TopNav />
            <main id="console-main" className="scrollbar-thin flex-1 overflow-y-auto">
              {children}
            </main>
          </div>
        </div>
      </body>
    </html>
  )
}
