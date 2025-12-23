import { defineConfig } from 'vitepress'

// https://vitepress.dev/reference/site-config
export default defineConfig({
  title: "Trusted Server",
  description: "Privacy-preserving edge computing for ad serving and synthetic ID generation",
  themeConfig: {
    // https://vitepress.dev/reference/default-theme-config
    nav: [
      { text: 'Home', link: '/' },
      { text: 'Guide', link: '/guide/getting-started' },
    ],

    sidebar: [
      {
        text: 'Introduction',
        items: [
          { text: 'What is Trusted Server?', link: '/guide/what-is-trusted-server' },
          { text: 'Getting Started', link: '/guide/getting-started' }
        ]
      },
      {
        text: 'Core Concepts',
        items: [
          { text: 'Synthetic IDs', link: '/guide/synthetic-ids' },
          { text: 'GDPR Compliance', link: '/guide/gdpr-compliance' },
          { text: 'Ad Serving', link: '/guide/ad-serving' }
        ]
      },
      {
        text: 'Security',
        items: [
          { text: 'Request Signing', link: '/guide/request-signing' },
          { text: 'Key Rotation', link: '/guide/key-rotation' }
        ]
      },
      {
        text: 'Development',
        items: [
          { text: 'Architecture', link: '/guide/architecture' },
          { text: 'Configuration', link: '/guide/configuration' },
          { text: 'Testing', link: '/guide/testing' },
          { text: 'Integration Guide', link: '/guide/integration-guide' }
        ]
      }
    ],

    socialLinks: [
      { icon: 'github', link: 'https://github.com/IABTechLab/trusted-server' }
    ],

    footer: {
      message: 'Released under the Apache License 2.0.',
      copyright: 'Copyright © 2018-present IAB Technology Laboratory'
    }
  }
})
