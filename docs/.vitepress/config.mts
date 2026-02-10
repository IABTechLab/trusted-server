import { defineConfig } from 'vitepress'
import { withMermaid } from 'vitepress-plugin-mermaid'
import { readFileSync } from 'node:fs'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))

// Parse .tool-versions file to extract version numbers
function getToolVersions(): Record<string, string> {
  const toolVersionsPath = resolve(__dirname, '../../.tool-versions')
  const versions: Record<string, string> = {}

  try {
    const content = readFileSync(toolVersionsPath, 'utf-8')
    for (const line of content.split('\n')) {
      const trimmed = line.trim()
      if (trimmed && !trimmed.startsWith('#')) {
        const [tool, version] = trimmed.split(/\s+/)
        if (tool && version) {
          versions[tool] = version
        }
      }
    }
  } catch (e) {
    console.warn('Could not read .tool-versions file:', e)
  }

  return versions
}

const toolVersions = getToolVersions()

// https://vitepress.dev/reference/site-config
export default withMermaid(
  defineConfig({
    title: 'Trusted Server',
    description:
      'Privacy-preserving edge computing for ad serving and synthetic ID generation',
    base: '/trusted-server',

    // Replace version placeholders like {{NODEJS_VERSION}} with values from .tool-versions
    markdown: {
      config: (md) => {
        const originalParse = md.parse.bind(md)
        md.parse = (src: string, env: Record<string, unknown>) => {
          let processed = src
          for (const [tool, version] of Object.entries(toolVersions)) {
            const placeholder = `{{${tool.toUpperCase()}_VERSION}}`
            processed = processed.replaceAll(placeholder, version)
          }
          return originalParse(processed, env)
        }
      },
    },

    themeConfig: {
      // https://vitepress.dev/reference/default-theme-config
      nav: [
        { text: 'Home', link: '/' },
        { text: 'Guide', link: '/guide/getting-started' },
        { text: 'Business Value', link: '/business-use-cases' },
        { text: 'Roadmap', link: '/roadmap' },
      ],

      sidebar: [
        {
          text: 'Introduction',
          items: [
            {
              text: 'What is Trusted Server?',
              link: '/guide/what-is-trusted-server',
            },
            { text: 'Getting Started', link: '/guide/getting-started' },
          ],
        },
        {
          text: 'Core Concepts',
          items: [
            { text: 'Synthetic IDs', link: '/guide/synthetic-ids' },
            { text: 'GDPR Compliance', link: '/guide/gdpr-compliance' },
            { text: 'Ad Serving', link: '/guide/ad-serving' },
            {
              text: 'Auction Orchestration',
              link: '/guide/auction-orchestration',
            },
            { text: 'First-Party Proxy', link: '/guide/first-party-proxy' },
            { text: 'Creative Processing', link: '/guide/creative-processing' },
            {
              text: 'Integrations Overview',
              link: '/guide/integrations-overview',
            },
          ],
        },
        {
          text: 'Security',
          items: [
            { text: 'Request Signing', link: '/guide/request-signing' },
            { text: 'Key Rotation', link: '/guide/key-rotation' },
          ],
        },
        {
          text: 'Development',
          items: [
            { text: 'Architecture', link: '/guide/architecture' },
            { text: 'Configuration', link: '/guide/configuration' },
            { text: 'Testing', link: '/guide/testing' },
            { text: 'Integration Guide', link: '/guide/integration-guide' },
          ],
        },
        {
          text: 'Advanced',
          items: [
            { text: 'RSC Hydration', link: '/guide/rsc-hydration' },
            {
              text: 'Proxy Signing',
              link: '/guide/proxy-signing',
            },
            { text: 'Collective Sync', link: '/guide/collective-sync' },
          ],
        },
        {
          text: 'Reference',
          items: [
            { text: 'API Reference', link: '/guide/api-reference' },
            { text: 'Error Reference', link: '/guide/error-reference' },
          ],
        },
        {
          text: 'Partner Integrations',
          items: [
            {
              text: 'Identity',
              items: [{ text: 'Lockr', link: '/guide/integrations/lockr' }],
            },
            {
              text: 'CMP',
              items: [{ text: 'Didomi', link: '/guide/integrations/didomi' }],
            },
            {
              text: 'Data',
              items: [
                { text: 'Permutive', link: '/guide/integrations/permutive' },
              ],
            },
            {
              text: 'Ad Serving',
              items: [{ text: 'GAM', link: '/guide/integrations/gam' }],
            },
            {
              text: 'Demand Wrapper',
              items: [
                { text: 'Prebid', link: '/guide/integrations/prebid' },
                { text: 'APS', link: '/guide/integrations/aps' },
              ],
            },
            {
              text: 'SSP',
              items: [{ text: 'Kargo', link: '/guide/integrations/kargo' }],
            },
            {
              text: 'Framework Support',
              items: [{ text: 'Next.js', link: '/guide/integrations/nextjs' }],
            },
            {
              text: 'Security',
              items: [
                { text: 'DataDome', link: '/guide/integrations/datadome' },
              ],
            },
          ],
        },
      ],

      socialLinks: [
        {
          icon: 'github',
          link: 'https://github.com/IABTechLab/trusted-server',
        },
      ],

      footer: {
        message: 'Released under the Apache License 2.0.',
        copyright: 'Copyright © 2018-present IAB Technology Laboratory',
      },
    },
    // Mermaid configuration
    mermaid: {
      flowchart: {
        useMaxWidth: true,
      },
      // https://mermaid.js.org/config/setup/modules/mermaidAPI.html#mermaidapi-configuration-defaults
    },
    mermaidPlugin: {
      class: 'mermaid',
    },
  })
)
