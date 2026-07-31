// ESLint v9 flat config
import js from '@eslint/js';
import globals from 'globals';
import tseslint from 'typescript-eslint';
import importPlugin from 'eslint-plugin-import';
import jsdoc from 'eslint-plugin-jsdoc';
import unicorn from 'eslint-plugin-unicorn';

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
    languageOptions: {
      parser: tseslint.parser,
      parserOptions: {
        ecmaVersion: 'latest',
        sourceType: 'module',
      },
    },
    plugins: {
      import: importPlugin,
      jsdoc,
      unicorn,
      '@typescript-eslint': tseslint.plugin,
    },
    rules: {
      'unicorn/prevent-abbreviations': 'off',
      'unicorn/filename-case': 'off',
      'import/order': ['error', { 'newlines-between': 'always' }],
    },
  },
  // Honor the `_`-prefix convention for intentionally unused bindings in every
  // linted file — tseslint recommended enables the rule globally, so scoping
  // this to *.ts(x) would leave .mjs at the pattern-less defaults
  {
    rules: {
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_', caughtErrorsIgnorePattern: '^_' },
      ],
    },
  },
  // Node build scripts and Node-run test harnesses — nodeBuiltin, not node,
  // so CommonJS-only names (__dirname, require, module) still fail no-undef
  // in these ES modules
  {
    files: ['*.mjs', 'test/**/*.mjs'],
    languageOptions: {
      globals: globals.nodeBuiltin,
    },
  },
];
