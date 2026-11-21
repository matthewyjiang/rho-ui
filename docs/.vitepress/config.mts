import { withMermaid } from 'vitepress-plugin-mermaid'
import type { DefaultTheme } from 'vitepress'

const guide: DefaultTheme.SidebarItem[] = [
  {
    text: 'Start',
    items: [
      { text: 'Overview', link: '/' },
      { text: 'Getting started', link: '/guide/getting-started' },
      { text: 'Keyboard shortcuts', link: '/guide/keys' },
    ],
  },
  {
    text: 'Use rho-ui',
    items: [
      { text: 'Sessions', link: '/guide/sessions' },
      { text: 'Working with a turn', link: '/guide/turns' },
      { text: 'The right panel', link: '/guide/panel' },
      { text: 'Remote machines', link: '/guide/machines' },
      { text: 'In a browser', link: '/guide/browser' },
      { text: 'Settings', link: '/guide/settings' },
    ],
  },
  {
    text: 'Reference',
    items: [
      { text: 'Architecture', link: '/architecture' },
      { text: 'Testing', link: '/testing' },
    ],
  },
]

export default withMermaid({
  title: 'rho-ui',
  description: 'The desktop app for rho.',
  base: '/rho-ui/',
  cleanUrls: true,
  lastUpdated: true,
  head: [['link', { rel: 'icon', type: 'image/svg+xml', href: '/rho-ui/icon.svg' }]],
  mermaid: {
    securityLevel: 'strict',
    theme: 'neutral',
  },
  mermaidPlugin: {
    class: 'rho-mermaid',
  },
  // mermaid >= 11.17 imports the CJS-only fastdom modules, which
  // vitepress-plugin-mermaid does not pre-bundle. Without this the dev
  // server serves raw CJS and the app fails to mount (blank page).
  vite: {
    optimizeDeps: {
      include: ['mermaid > fastdom', 'mermaid > fastdom/extensions/fastdom-promised.js'],
    },
  },
  themeConfig: {
    logo: '/icon.svg',
    nav: [
      { text: 'Getting started', link: '/guide/getting-started' },
      { text: 'Guide', link: '/guide/sessions' },
      { text: 'Reference', link: '/architecture' },
      { text: 'rho', link: 'https://matthewyjiang.github.io/rho/' },
    ],
    sidebar: guide,
    socialLinks: [{ icon: 'github', link: 'https://github.com/matthewyjiang/rho-ui' }],
    search: { provider: 'local' },
    editLink: {
      pattern: 'https://github.com/matthewyjiang/rho-ui/edit/main/docs/:path',
      text: 'Edit this page on GitHub',
    },
    outline: { level: [2, 3] },
  },
})
