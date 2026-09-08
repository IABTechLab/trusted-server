import { defineConfig } from 'vitepress'
import { withMermaid } from 'vitepress-plugin-mermaid'
import { readFileSync } from 'node:fs'
import { resolve, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = dirname(fileURLToPath(import.meta.url))
const buildSha = process.env.GITHUB_SHA?.trim()

if (buildSha && !/^[0-9a-f]{40}$/.test(buildSha)) {
  throw new Error('GITHUB_SHA must be a lowercase 40-character commit SHA')
}

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
  } catch (error) {
    console.warn('Could not read .tool-versions file:', error)
  }

  return versions
}

function provenanceBanner(): string {
  const source = buildSha
    ? `Built from commit \`${buildSha}\`.`
    : 'Built from a local checkout; no immutable commit SHA was supplied.'
  return `::: info Rolling main documentation\n${source} Content may describe unreleased behavior.\n:::`
}

const toolVersions = getToolVersions()

export default withMermaid(
  defineConfig({
    title: 'Trusted Server',
    description:
      'Edge computing for ad serving, consent signal handling, and edge cookie (EC) generation',
    base: '/trusted-server',
    lastUpdated: true,
    srcExclude: [
      'superpowers/**',
      'internal/**',
      'epics/**',
      'guide/onboarding.md',
      'README.md',
      'business-use-cases.md',
    ],

    markdown: {
      config: (md) => {
        const originalParse = md.parse.bind(md)
        md.parse = (src: string, env: Record<string, unknown>) => {
          let processed = src
          for (const [tool, version] of Object.entries(toolVersions)) {
            const placeholder = `{{${tool.toUpperCase()}_VERSION}}`
            processed = processed.replaceAll(placeholder, version)
          }
          return originalParse(`${provenanceBanner()}\n\n${processed}`, env)
        }
      },
    },

    themeConfig: {
      search: {
        provider: 'local',
      },
      nav: [
        { text: 'Home', link: '/' },
        { text: 'Guide', link: '/guide/' },
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
          text: 'Operator',
          items: [
            { text: 'EdgeZero Lifecycle', link: '/guide/edgezero' },
            { text: 'Configuration', link: '/guide/configuration' },
            { text: 'CLI', link: '/guide/cli' },
            { text: 'Dev Proxy', link: '/guide/ts-dev-proxy' },
            { text: 'Testing', link: '/guide/testing' },
            { text: 'Auction Testing', link: '/guide/auction-testing' },
          ],
        },
        {
          text: 'Product',
          items: [
            { text: 'Edge Cookies', link: '/guide/edge-cookies' },
            { text: 'EC Setup', link: '/guide/ec-setup-guide' },
            { text: 'GDPR Compliance', link: '/guide/gdpr-compliance' },
            { text: 'Ad Serving', link: '/guide/ad-serving' },
            {
              text: 'Auction Orchestration',
              link: '/guide/auction-orchestration',
            },
            { text: 'First-Party Proxy', link: '/guide/first-party-proxy' },
            { text: 'Asset Routes', link: '/guide/asset-routes' },
            { text: 'Creative Processing', link: '/guide/creative-processing' },
            { text: 'Trusted Server JavaScript', link: '/guide/tsjs' },
            { text: 'Auction Telemetry', link: '/guide/telemetry' },
            { text: 'RSC Hydration', link: '/guide/rsc-hydration' },
            {
              text: 'Edge Cookie External Sync',
              link: '/guide/collective-sync',
            },
            { text: 'Request Signing', link: '/guide/request-signing' },
            { text: 'Key Rotation', link: '/guide/key-rotation' },
            { text: 'Proxy Signing', link: '/guide/proxy-signing' },
          ],
        },
        {
          text: 'Deployment',
          items: [
            { text: 'Fastly', link: '/guide/fastly' },
            { text: 'Axum Development', link: '/guide/axum-dev' },
            { text: 'Cloudflare Workers', link: '/guide/cloudflare' },
            { text: 'Fermyon Spin', link: '/guide/spin' },
          ],
        },
        {
          text: 'Integrations',
          items: [
            {
              text: 'Integration Inventory',
              link: '/guide/integrations-overview',
            },
            { text: 'Development Guide', link: '/guide/integration-guide' },
            {
              text: 'Ad Server Mock',
              link: '/guide/integrations/adserver_mock',
            },
            { text: 'APS', link: '/guide/integrations/aps' },
            { text: 'DataDome', link: '/guide/integrations/datadome' },
            { text: 'Didomi', link: '/guide/integrations/didomi' },
            {
              text: 'Google Tag Manager',
              link: '/guide/integrations/google_tag_manager',
            },
            { text: 'GPT', link: '/guide/integrations/gpt' },
            {
              text: 'GPT Runtime Diagnostics',
              link: '/guide/integrations/gpt-diagnostics',
            },
            { text: 'Lockr', link: '/guide/integrations/lockr' },
            { text: 'Next.js', link: '/guide/integrations/nextjs' },
            { text: 'Osano', link: '/guide/integrations/osano' },
            { text: 'Permutive', link: '/guide/integrations/permutive' },
            { text: 'Prebid', link: '/guide/integrations/prebid' },
            { text: 'Sourcepoint', link: '/guide/integrations/sourcepoint' },
            { text: 'Testlight', link: '/guide/integrations/testlight' },
          ],
        },
        {
          text: 'Reference',
          items: [
            { text: 'Architecture', link: '/guide/architecture' },
            { text: 'API Reference', link: '/guide/api-reference' },
            { text: 'Error Reference', link: '/guide/error-reference' },
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
    mermaid: {
      flowchart: {
        useMaxWidth: true,
      },
    },
    mermaidPlugin: {
      class: 'mermaid',
    },
  })
)
