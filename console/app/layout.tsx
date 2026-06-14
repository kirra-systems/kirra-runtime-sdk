import type { Metadata } from 'next'
import { Space_Grotesk, Inter, JetBrains_Mono } from 'next/font/google'
import './globals.css'
import { Sidebar } from '@/components/shell/sidebar'
import { TopNav } from '@/components/shell/top-nav'

const display = Space_Grotesk({ subsets: ['latin'], variable: '--font-display' })
const sans = Inter({ subsets: ['latin'], variable: '--font-sans' })
const mono = JetBrains_Mono({ subsets: ['latin'], variable: '--font-mono' })

export const metadata: Metadata = {
  title: 'Kirra Mission Console',
  description: 'Fleet operations & AI safety governance for autonomous systems.',
}

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" className={`${display.variable} ${sans.variable} ${mono.variable} dark`}>
      <body className="font-sans antialiased">
        <div className="flex h-screen overflow-hidden bg-bg">
          <Sidebar />
          <div className="flex min-w-0 flex-1 flex-col">
            <TopNav />
            <main className="scrollbar-thin flex-1 overflow-y-auto">{children}</main>
          </div>
        </div>
      </body>
    </html>
  )
}
