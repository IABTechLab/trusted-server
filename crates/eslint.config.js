// Editors look for an ESLint config above the file they lint. This one applies
// the shared browser code's rules to module scripts kept in a crate's
// js/src and js/test folders, and to nothing else under crates.
import shared from './trusted-server-js/lib/eslint.config.js';

export default [
  {
    ignores: ['**/*', '!*/', '!*/*/', '!*/*/js/', '!*/*/js/src/**', '!*/*/js/test/**'],
  },
  ...shared,
];
