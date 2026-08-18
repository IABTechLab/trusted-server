// ESLint v10 flat config
import js from '@eslint/js';
import { createTypeScriptImportResolver } from 'eslint-import-resolver-typescript';
import importX from 'eslint-plugin-import-x';
import globals from 'globals';
import tseslint from 'typescript-eslint';
import jsdoc from 'eslint-plugin-jsdoc';
import unicorn from 'eslint-plugin-unicorn';

import noAdtechGlobals from './eslint-rules/no-adtech-globals.js';

export const ARCHITECTURE_INTEGRATION_DIRECTORIES = Object.freeze([
  'aps',
  'creative',
  'datadome',
  'didomi',
  'google_tag_manager',
  'gpt',
  'gpt_diagnostics',
  'lockr',
  'osano',
  'permutive',
  'prebid',
  'render_runtime',
  'sourcepoint',
  'testlight',
]);

const integrationIsolationZones = ARCHITECTURE_INTEGRATION_DIRECTORIES.map((integration) => ({
  target: `./src/integrations/${integration}`,
  from: './src/integrations',
  except: [`./${integration}`],
  message: 'Integrations must compose through injected services, not import another integration.',
}));

export const ARCHITECTURE_RESTRICTED_LAYER_ZONES = Object.freeze([
  {
    target: './src/core',
    from: ['./src/adapters', './src/services', './src/integrations', './src/composition'],
    message: 'Core must not construct or import downstream architecture layers.',
  },
  {
    target: './src/kernel',
    from: ['./src/adapters', './src/services', './src/integrations', './src/composition'],
    message: 'Kernel may depend only on kernel contracts.',
  },
  {
    target: './src/adapters',
    from: [
      './src/core',
      './src/shared',
      './src/services',
      './src/integrations',
      './src/composition',
    ],
    message: 'Adapters may depend only on kernel contracts.',
  },
  {
    target: './src/services',
    from: ['./src/core', './src/shared', './src/integrations', './src/composition'],
    message: 'Services may depend only on kernel and adapter contracts.',
  },
  {
    target: './src/integrations',
    from: './src/composition',
    message: 'Integrations must not depend on the composition root.',
  },
  {
    target: ['./src/kernel', './src/adapters', './src/services', './src/integrations'],
    from: './src/index.ts',
    message: 'Lower architecture layers must not bypass boundaries through the root barrel.',
  },
  ...integrationIsolationZones,
]);

export default [
  // Files/folders to ignore
  {
    ignores: ['node_modules', 'dist', 'coverage'],
  },
  // Base JS recommended
  js.configs.recommended,
  // TypeScript recommended
  ...tseslint.configs.recommended,
  // Project rules
  {
    files: ['**/*.ts', '**/*.tsx'],
    settings: {
      'import-x/resolver-next': [
        createTypeScriptImportResolver({
          project: './tsconfig.json',
        }),
      ],
    },
    languageOptions: {
      parser: tseslint.parser,
      parserOptions: {
        ecmaVersion: 'latest',
        sourceType: 'module',
      },
    },
    plugins: {
      'import-x': importX,
      jsdoc,
      tsjs: {
        rules: {
          'no-adtech-globals': noAdtechGlobals,
        },
      },
      unicorn,
      '@typescript-eslint': tseslint.plugin,
    },
    rules: {
      'unicorn/prevent-abbreviations': 'off',
      'unicorn/filename-case': 'off',
      'import-x/order': ['error', { 'newlines-between': 'always' }],
    },
  },
  {
    files: ['src/**/*.ts', 'src/**/*.tsx'],
    rules: {
      'tsjs/no-adtech-globals': [
        'error',
        {
          rootDirectory: import.meta.dirname,
        },
      ],
    },
  },
  {
    files: ['src/**/*.ts', 'src/**/*.tsx'],
    rules: {
      'import-x/no-restricted-paths': [
        'error',
        {
          basePath: import.meta.dirname,
          zones: ARCHITECTURE_RESTRICTED_LAYER_ZONES,
        },
      ],
    },
  },
  // Honor the `_`-prefix convention for intentionally unused bindings in every
  // linted file: tseslint recommended enables the rule globally, so scoping
  // this to *.ts(x) would leave .mjs at the pattern-less defaults
  {
    rules: {
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_', caughtErrorsIgnorePattern: '^_' },
      ],
    },
  },
  // Node build scripts and Node-run test harnesses use nodeBuiltin, not node,
  // so CommonJS-only names (__dirname, require, module) still fail no-undef
  // in these ES modules
  {
    files: ['*.mjs', 'scripts/**/*.mjs', 'test/**/*.mjs'],
    languageOptions: {
      globals: globals.nodeBuiltin,
    },
  },
];
