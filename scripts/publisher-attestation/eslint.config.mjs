import { createRequire } from 'node:module'

const require = createRequire(
  new URL('../../docs/package.json', import.meta.url)
)
const js = require('@eslint/js')

export default [
  js.configs.recommended,
  {
    rules: { 'no-template-curly-in-string': 'error' },
    languageOptions: {
      globals: Object.fromEntries(
        ['Buffer', 'URL', 'TextDecoder', 'process', 'structuredClone'].map(
          (name) => [name, 'readonly']
        )
      ),
    },
  },
]
