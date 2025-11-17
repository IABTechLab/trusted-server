# tsjs - Trusted Server JavaScript Library

Unified JavaScript library with queue-based API and modular architecture for ad serving, creative protection, and third-party integrations.

## Architecture

The library uses a **conditional compilation** system that allows building a single unified bundle with only the modules you need, instead of loading multiple separate bundles.

### Available Modules

- **core** - Core API (queue, config, ad units, rendering) - *Always included*
- **ext** - Extensions (Prebid.js integration)
- **creative** - Creative runtime guards (click protection, render guards)
- **permutive** - Permutive SDK proxy for first-party data

## Building

Build a single unified bundle with the modules you need:

```bash
# Default: full featured bundle (all modules) - ~24 KB
npm run build

# Minimal bundle (core only) - ~9.4 KB
npm run build:minimal

# Custom combination
TSJS_MODULES=core,ext,creative npm run build:custom
```

### Bundle Size Options

| Configuration | Size | Gzipped | Use Case |
|--------------|------|---------|----------|
| `core` only | 9.4 KB | 4.0 KB | Minimal ad serving |
| `core,ext` | 10.8 KB | 4.4 KB | Ad serving + Prebid |
| `core,creative` | 21.8 KB | 7.7 KB | Ad serving + protection |
| `core,ext,creative,permutive` | 24.0 KB | 8.4 KB | Full featured (default) |

## Development

```bash
# Watch mode (rebuilds on changes)
npm run dev

# Run tests
npm test

# Lint
npm run lint
```

## How It Works

### Auto-Discovery Plugin

The Vite plugin (`tsjs-module-discovery`) automatically:

1. Scans `src/` for directories containing `index.ts`
2. Filters based on `TSJS_MODULES` environment variable
3. Generates `src/generated-modules.ts` with conditional imports
4. Only imports specified modules → Rollup tree-shakes everything else

### Generated Code

When you build with `TSJS_MODULES=core,creative`, the plugin generates:

```typescript
// src/generated-modules.ts (auto-generated)
import * as core from './core/index';
import * as creative from './creative/index';

export const modules = {
  core,
  creative,
};
```

Modules not listed (like `ext` and `permutive`) are never imported, so they're completely removed from the bundle by Rollup's tree-shaking.

### Entry Point

The unified entry point (`src/index.ts`) imports the generated modules and initializes them:

```typescript
import { modules } from './generated-modules';

// Core module self-initializes on import
// Other modules are logged and made available
```

## Adding New Modules

To add a new module:

1. Create a new directory in `src/` (e.g., `src/mymodule/`)
2. Add an `index.ts` file that exports your module's API
3. The plugin will automatically discover it
4. Include it in builds: `TSJS_MODULES=core,mymodule`

No other configuration needed!

## Environment Variables

- `TSJS_UNIFIED` - Set to `true` to use unified bundle mode
- `TSJS_MODULES` - Comma-separated list of modules to include (e.g., `core,ext,creative`)
- `TSJS_BUNDLE` - (Legacy) Specifies which individual bundle to build

## Testing

```bash
# Run all tests
npm test

# Watch mode
npm run test:watch
```

## Code Quality

```bash
# Format check
npm run format

# Format fix
npm run format:write

# Lint
npm run lint

# Lint fix
npm run lint:fix
```

## Project Structure

```
src/
  core/          - Core API (queue, config, rendering)
  creative/      - Creative protection (click guards, render guards)
  ext/           - Extensions (Prebid.js integration)
  permutive/     - Permutive SDK proxy
  shared/        - Shared utilities (async helpers, scheduler)
  index.ts       - Unified bundle entry point
  generated-modules.ts - Auto-generated (git-ignored)
```

## Benefits

- **Single bundle** - One JavaScript file instead of multiple
- **Conditional compilation** - Only includes modules you need
- **Smaller size** - Shared code included once, tree-shaking removes unused code
- **Better caching** - Single file to cache
- **Flexible** - Build minimal (9.4 KB) to full featured (24 KB)
