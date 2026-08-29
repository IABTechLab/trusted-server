// @vitest-environment node

import crypto from 'node:crypto';
import fs from 'node:fs';
import { createRequire } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it, vi } from 'vitest';

import {
  createSelectionManifest,
  deriveBundleMetadata,
  main,
  normalizeModuleRequest,
  parseArgs,
  parseModuleRequest,
  renderExternalEntry,
  renderGeneratedModules,
  resolveBundleModules,
  verifyPrebidPackageVersion,
} from '../build-prebid-external.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const libDir = path.resolve(__dirname, '..');
const prebidPackageDir = path.join(libDir, 'node_modules', 'prebid.js');
const prebidMetadataDir = path.join(prebidPackageDir, 'metadata', 'modules');
const registry = JSON.parse(
  fs.readFileSync(
    path.join(libDir, 'src', 'integrations', 'prebid', 'user_id_modules.json'),
    'utf8'
  )
);

function completeRequest(overrides = {}) {
  return {
    bidder: ['rubiconBidAdapter'],
    userId: ['sharedIdSystem'],
    analytics: ['atsAnalyticsAdapter'],
    ...overrides,
  };
}

function parseRequest(value) {
  return parseModuleRequest(JSON.stringify(value));
}

function actualResolveOptions(overrides = {}) {
  return {
    prebidVersion: '10.26.0',
    registry,
    metadataDir: prebidMetadataDir,
    packageDir: prebidPackageDir,
    ...overrides,
  };
}

function createResolverFixture(metadata) {
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-resolver-'));
  const packageDir = path.join(temp, 'prebid.js');
  const metadataDir = path.join(packageDir, 'metadata', 'modules');
  const target = path.join(packageDir, 'dist', 'src', 'public', 'exampleModule.js');
  fs.mkdirSync(metadataDir, { recursive: true });
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(path.join(metadataDir, 'exampleModule.json'), JSON.stringify(metadata));
  fs.writeFileSync(target, 'export {};\n');
  return { temp, packageDir, metadataDir, target };
}

describe('build-prebid-external request parsing', () => {
  it('accepts the typed module request and preserves order', () => {
    const request = parseRequest({
      bidder: ['rubiconBidAdapter', 'kargoBidAdapter'],
      userId: ['sharedIdSystem', 'uid2IdSystem'],
      analytics: ['atsAnalyticsAdapter'],
    });

    expect(request).toEqual({
      bidder: ['rubiconBidAdapter', 'kargoBidAdapter'],
      userId: ['sharedIdSystem', 'uid2IdSystem'],
      analytics: ['atsAnalyticsAdapter'],
    });
  });

  it('expands omitted User ID modules and normalizes omitted analytics', () => {
    const request = parseRequest({ bidder: ['rubiconBidAdapter'] });

    expect(normalizeModuleRequest(request, ['sharedIdSystem', 'uid2IdSystem'])).toEqual({
      bidder: ['rubiconBidAdapter'],
      userId: ['sharedIdSystem', 'uid2IdSystem'],
      analytics: [],
    });
  });

  it('preserves explicit empty optional selections', () => {
    const request = parseRequest({
      bidder: ['rubiconBidAdapter'],
      userId: [],
      analytics: [],
    });

    expect(normalizeModuleRequest(request, ['sharedIdSystem'])).toEqual({
      bidder: ['rubiconBidAdapter'],
      userId: [],
      analytics: [],
    });
  });

  it.each([
    [{}, 'modules.bidder'],
    [{ bidder: [] }, 'modules.bidder'],
    [{ bidder: 'rubiconBidAdapter' }, 'modules.bidder'],
    [{ bidder: ['rubiconBidAdapter'], userId: 'sharedIdSystem' }, 'modules.user_id'],
    [{ bidder: ['rubiconBidAdapter'], analytics: [42] }, 'modules.analytics'],
    [{ bidder: ['rubiconBidAdapter'], extra: [] }, 'unsupported field "extra"'],
    [[], 'JSON object'],
    [null, 'JSON object'],
  ])('rejects malformed request %#', (request, expectedMessage) => {
    expect(() => parseRequest(request)).toThrow(expectedMessage);
  });

  it.each([
    '',
    ' ',
    'atsAnalyticsAdapter.js',
    '../atsAnalyticsAdapter',
    'group/atsAnalyticsAdapter',
    'group\\atsAnalyticsAdapter',
    'https://example.com/adapter',
    "atsAnalyticsAdapter';alert(1)//",
    'ats\nAnalyticsAdapter',
  ])('rejects invalid analytics stem %j', (stem) => {
    expect(() => parseRequest(completeRequest({ analytics: [stem] }))).toThrow(
      'integrations.prebid.bundle.modules.analytics'
    );
  });

  it('rejects duplicates within a kind', () => {
    expect(() =>
      parseRequest(completeRequest({ analytics: ['atsAnalyticsAdapter', 'atsAnalyticsAdapter'] }))
    ).toThrow('duplicate module stem "atsAnalyticsAdapter"');
  });

  it('rejects a stem repeated across kinds', () => {
    expect(() => parseRequest({ bidder: ['exampleModule'], analytics: ['exampleModule'] })).toThrow(
      'already selected by integrations.prebid.bundle.modules.bidder'
    );
  });

  it('rejects malformed JSON with the generator prefix', () => {
    expect(() => parseModuleRequest('{')).toThrow(
      '[build-prebid-external] --modules-json must contain valid JSON'
    );
  });

  it('parses both option forms and resolves relative output paths', () => {
    const json = JSON.stringify({ bidder: ['rubiconBidAdapter'] });

    expect(parseArgs([`--modules-json=${json}`, '--out=dist/prebid']).outDir).toBe(
      path.resolve(process.cwd(), 'dist/prebid')
    );
    expect(
      parseArgs(['--modules-json', json, '--out', 'other/prebid']).moduleRequest.bidder
    ).toEqual(['rubiconBidAdapter']);
  });

  it.each([
    [[], 'Missing required --modules-json'],
    [['--adapters', 'rubicon'], 'Unknown option --adapters'],
    [['--user-id-modules', 'sharedIdSystem'], 'Unknown option --user-id-modules'],
    [['--unknown', 'value'], 'Unknown option --unknown'],
    [['positional'], 'Unexpected positional argument'],
    [['--modules-json'], 'Missing value for --modules-json'],
    [
      [
        '--modules-json',
        JSON.stringify({ bidder: ['rubiconBidAdapter'] }),
        '--modules-json',
        JSON.stringify({ bidder: ['kargoBidAdapter'] }),
      ],
      'may only be specified once',
    ],
  ])('rejects invalid arguments %#', (argv, expectedMessage) => {
    expect(() => parseArgs(argv)).toThrow(expectedMessage);
  });
});

describe('build-prebid-external dependency validation', () => {
  function writeVersionFixtures(lockVersion, installedVersion) {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-version-'));
    const lockFile = path.join(temp, 'package-lock.json');
    const packageJsonFile = path.join(temp, 'package.json');
    fs.writeFileSync(
      lockFile,
      JSON.stringify({ packages: { 'node_modules/prebid.js': { version: lockVersion } } })
    );
    fs.writeFileSync(packageJsonFile, JSON.stringify({ version: installedVersion }));
    return { temp, lockFile, packageJsonFile };
  }

  it('returns the matching installed Prebid version', () => {
    const fixture = writeVersionFixtures('10.26.0', '10.26.0');
    try {
      expect(verifyPrebidPackageVersion(fixture)).toBe('10.26.0');
    } finally {
      fs.rmSync(fixture.temp, { recursive: true, force: true });
    }
  });

  it('rejects a lockfile/install mismatch with recovery guidance', () => {
    const fixture = writeVersionFixtures('10.26.0', '10.25.0');
    try {
      expect(() => verifyPrebidPackageVersion(fixture)).toThrow(
        'installed prebid.js version 10.25.0 does not match package-lock.json version 10.26.0'
      );
      expect(() => verifyPrebidPackageVersion(fixture)).toThrow('npm ci');
    } finally {
      fs.rmSync(fixture.temp, { recursive: true, force: true });
    }
  });

  it.each([
    [{ packages: {} }, { version: '10.26.0' }, 'does not declare packages'],
    [
      { packages: { 'node_modules/prebid.js': { version: '10.26.0' } } },
      {},
      'does not declare a Prebid version',
    ],
  ])('rejects missing version data %#', (lock, installed, expectedMessage) => {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-version-'));
    const lockFile = path.join(temp, 'package-lock.json');
    const packageJsonFile = path.join(temp, 'package.json');
    fs.writeFileSync(lockFile, JSON.stringify(lock));
    fs.writeFileSync(packageJsonFile, JSON.stringify(installed));
    try {
      expect(() => verifyPrebidPackageVersion({ lockFile, packageJsonFile })).toThrow(
        expectedMessage
      );
    } finally {
      fs.rmSync(temp, { recursive: true, force: true });
    }
  });

  it('rejects malformed dependency JSON', () => {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-version-'));
    const lockFile = path.join(temp, 'package-lock.json');
    const packageJsonFile = path.join(temp, 'package.json');
    fs.writeFileSync(lockFile, '{');
    fs.writeFileSync(packageJsonFile, JSON.stringify({ version: '10.26.0' }));
    try {
      expect(() => verifyPrebidPackageVersion({ lockFile, packageJsonFile })).toThrow(
        'could not parse npm lockfile'
      );
    } finally {
      fs.rmSync(temp, { recursive: true, force: true });
    }
  });
});

describe('build-prebid-external module resolution', () => {
  it('resolves bidder, User ID, and analytics modules through package exports', () => {
    const resolved = resolveBundleModules(completeRequest(), actualResolveOptions());
    const manifest = createSelectionManifest(resolved);

    expect(manifest).toEqual({
      schemaVersion: 1,
      modules: {
        bidder: ['rubiconBidAdapter'],
        userId: ['sharedIdSystem'],
        analytics: ['atsAnalyticsAdapter'],
      },
      runtimeCodes: {
        bidder: ['rubicon'],
        analytics: ['atsAnalytics'],
      },
    });
    expect(resolved.userId[0].specifier).toBe('prebid.js/modules/sharedIdSystem.js');
    expect(fs.existsSync(path.join(prebidPackageDir, 'modules', 'sharedIdSystem.js'))).toBe(false);
  });

  it('resolves every module in the curated default User ID preset', () => {
    const selection = normalizeModuleRequest(
      parseRequest({ bidder: ['rubiconBidAdapter'] }),
      registry.defaultPreset
    );

    const resolved = resolveBundleModules(selection, actualResolveOptions());

    expect(resolved.userId.map(({ stem }) => stem)).toEqual(registry.defaultPreset);
  });

  it('validates the fixed LiveIntent shim and package targets after upstream resolution', () => {
    const resolved = resolveBundleModules(
      completeRequest({ userId: ['liveIntentIdSystem'], analytics: [] }),
      actualResolveOptions()
    );

    expect(resolved.userId[0].specifier).toBe('prebid.js/modules/liveIntentIdSystem.js');
  });

  it('derives every bidder alias and sorts runtime codes', () => {
    const resolved = resolveBundleModules(
      completeRequest({ bidder: ['adfBidAdapter'], analytics: [] }),
      actualResolveOptions()
    );

    expect(createSelectionManifest(resolved).runtimeCodes.bidder).toEqual([
      'adf',
      'adform',
      'adformOpenRTB',
    ]);
  });

  it.each([
    [completeRequest({ analytics: ['sharedIdSystem'] }), 'declares userId rather than analytics'],
    [
      completeRequest({ userId: ['exampleIdSystem'], analytics: [] }),
      'Trusted Server has no User ID registry entry',
    ],
    [
      completeRequest({ analytics: ['ATSanAlyticsAdapter'] }),
      'does not provide modules/ATSanAlyticsAdapter.js',
    ],
  ])('rejects unsupported or wrong-kind selection %#', (selection, expectedMessage) => {
    expect(() => resolveBundleModules(selection, actualResolveOptions())).toThrow(expectedMessage);
  });

  it('rejects an unresolved or non-file package export', () => {
    expect(() =>
      resolveBundleModules(
        completeRequest({ userId: [], analytics: [] }),
        actualResolveOptions({
          resolveSpecifier: () => {
            throw new Error('example missing export');
          },
        })
      )
    ).toThrow('does not provide modules/rubiconBidAdapter.js');

    expect(() =>
      resolveBundleModules(
        completeRequest({ userId: [], analytics: [] }),
        actualResolveOptions({ resolveSpecifier: () => prebidPackageDir })
      )
    ).toThrow('does not resolve to a regular file');
  });

  it('rejects a package-export target outside the pinned package', () => {
    const fixture = createResolverFixture({
      components: [{ componentType: 'analytics', componentName: 'exampleAnalytics' }],
    });
    const escapedTarget = path.join(fixture.temp, 'escaped.js');
    fs.writeFileSync(escapedTarget, 'export {};\n');
    try {
      expect(() =>
        resolveBundleModules(
          { bidder: [], userId: [], analytics: ['exampleModule'] },
          actualResolveOptions({
            registry: { modules: [] },
            packageDir: fixture.packageDir,
            metadataDir: fixture.metadataDir,
            resolveSpecifier: () => escapedTarget,
          })
        )
      ).toThrow('resolves outside the pinned prebid.js package');
    } finally {
      fs.rmSync(fixture.temp, { recursive: true, force: true });
    }
  });

  it('rejects a metadata symlink that escapes the metadata directory', () => {
    const fixture = createResolverFixture({ components: [] });
    const externalMetadata = path.join(fixture.temp, 'outside.json');
    fs.writeFileSync(
      externalMetadata,
      JSON.stringify({
        components: [{ componentType: 'analytics', componentName: 'exampleAnalytics' }],
      })
    );
    fs.rmSync(path.join(fixture.metadataDir, 'exampleModule.json'));
    fs.symlinkSync(externalMetadata, path.join(fixture.metadataDir, 'exampleModule.json'));
    try {
      expect(() =>
        resolveBundleModules(
          { bidder: [], userId: [], analytics: ['exampleModule'] },
          actualResolveOptions({
            registry: { modules: [] },
            packageDir: fixture.packageDir,
            metadataDir: fixture.metadataDir,
            resolveSpecifier: () => fixture.target,
          })
        )
      ).toThrow('resolves outside the pinned metadata directory');
    } finally {
      fs.rmSync(fixture.temp, { recursive: true, force: true });
    }
  });

  it('rejects malformed metadata JSON with field and path context', () => {
    const fixture = createResolverFixture({ components: [] });
    fs.writeFileSync(path.join(fixture.metadataDir, 'exampleModule.json'), '{');
    try {
      expect(() =>
        resolveBundleModules(
          { bidder: [], userId: [], analytics: ['exampleModule'] },
          actualResolveOptions({
            registry: { modules: [] },
            packageDir: fixture.packageDir,
            metadataDir: fixture.metadataDir,
            resolveSpecifier: () => fixture.target,
          })
        )
      ).toThrow('Prebid metadata for integrations.prebid.bundle.modules.analytics');
    } finally {
      fs.rmSync(fixture.temp, { recursive: true, force: true });
    }
  });

  it.each([
    [{}, 'has no components array'],
    [{ components: 'analytics' }, 'has no components array'],
    [{ components: [{ componentType: 'analytics' }] }, 'without a non-empty componentName'],
    [
      {
        components: [
          { componentType: 'analytics', componentName: 'exampleAnalytics' },
          { componentType: 'analytics', componentName: 42 },
        ],
      },
      'without a non-empty componentName',
    ],
  ])('rejects malformed metadata %#', (metadata, expectedMessage) => {
    const fixture = createResolverFixture(metadata);
    try {
      expect(() =>
        resolveBundleModules(
          { bidder: [], userId: [], analytics: ['exampleModule'] },
          actualResolveOptions({
            registry: { modules: [] },
            packageDir: fixture.packageDir,
            metadataDir: fixture.metadataDir,
            resolveSpecifier: () => fixture.target,
          })
        )
      ).toThrow(expectedMessage);
    } finally {
      fs.rmSync(fixture.temp, { recursive: true, force: true });
    }
  });
});

describe('build-prebid-external rendering and orchestration', () => {
  it('renders imports in kind and configured order from one manifest', () => {
    const resolved = resolveBundleModules(
      {
        bidder: ['kargoBidAdapter', 'rubiconBidAdapter'],
        userId: ['uid2IdSystem', 'sharedIdSystem'],
        analytics: ['atsAnalyticsAdapter'],
      },
      actualResolveOptions()
    );
    const manifest = createSelectionManifest(resolved);
    const source = renderGeneratedModules(resolved, manifest);

    const specifiers = [
      'kargoBidAdapter.js',
      'rubiconBidAdapter.js',
      'uid2IdSystem.js',
      'sharedIdSystem.js',
      'atsAnalyticsAdapter.js',
    ];
    const offsets = specifiers.map((specifier) => source.indexOf(specifier));
    expect(offsets.every((offset) => offset >= 0)).toBe(true);
    expect(offsets).toEqual([...offsets].sort((left, right) => left - right));
    expect(source).toContain('"schemaVersion":1');
    expect(source).not.toContain('bidderCodes');
    expect(source).not.toContain('userIdModules');
  });

  it('omits User ID base and analytics imports for empty selections', () => {
    const resolved = resolveBundleModules(
      { bidder: ['rubiconBidAdapter'], userId: [], analytics: [] },
      actualResolveOptions()
    );
    const modulesSource = renderGeneratedModules(resolved, createSelectionManifest(resolved));
    const entrySource = renderExternalEntry({ includeUserIdModules: false });

    expect(modulesSource).not.toContain('AnalyticsAdapter.js');
    expect(entrySource).not.toContain('prebid.js/modules/userId.js');
  });

  it('derives filename, sha256, and SRI from exact bundle bytes', () => {
    const bundleBytes = Buffer.from('console.log("trusted prebid");\n', 'utf8');
    const sha256 = crypto.createHash('sha256').update(bundleBytes).digest('hex');
    const sri = `sha384-${crypto.createHash('sha384').update(bundleBytes).digest('base64')}`;

    expect(deriveBundleMetadata(bundleBytes)).toEqual({
      filename: `trusted-prebid-${sha256}.js`,
      sha256,
      sri,
    });
  });

  it('fails a dependency mismatch before creating generated paths or invoking Vite', async () => {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-mismatch-'));
    const lockFile = path.join(temp, 'package-lock.json');
    const packageJsonFile = path.join(temp, 'package.json');
    fs.writeFileSync(
      lockFile,
      JSON.stringify({ packages: { 'node_modules/prebid.js': { version: '10.26.0' } } })
    );
    fs.writeFileSync(packageJsonFile, JSON.stringify({ version: '10.25.0' }));
    const createGeneratedPaths = vi.fn();
    const buildBundle = vi.fn();
    try {
      await expect(
        main(
          [
            '--modules-json',
            JSON.stringify({ bidder: ['rubiconBidAdapter'] }),
            '--out',
            path.join(temp, 'out'),
          ],
          { lockFile, packageJsonFile, createGeneratedPaths, buildBundle }
        )
      ).rejects.toThrow('does not match package-lock.json version');
      expect(createGeneratedPaths).not.toHaveBeenCalled();
      expect(buildBundle).not.toHaveBeenCalled();
    } finally {
      fs.rmSync(temp, { recursive: true, force: true });
    }
  });

  it('fails an escaping metadata root before creating generated paths or invoking Vite', async () => {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-metadata-root-'));
    const packageDir = path.join(temp, 'prebid.js');
    const metadataParent = path.join(packageDir, 'metadata');
    const metadataDir = path.join(metadataParent, 'modules');
    const externalMetadataDir = path.join(temp, 'external-metadata');
    const target = path.join(packageDir, 'dist', 'src', 'public', 'rubiconBidAdapter.js');
    fs.mkdirSync(metadataParent, { recursive: true });
    fs.mkdirSync(externalMetadataDir, { recursive: true });
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(
      path.join(externalMetadataDir, 'rubiconBidAdapter.json'),
      JSON.stringify({
        components: [{ componentType: 'bidder', componentName: 'rubicon' }],
      })
    );
    fs.writeFileSync(target, 'export {};\n');
    fs.symlinkSync(externalMetadataDir, metadataDir);
    const createGeneratedPaths = vi.fn();
    const buildBundle = vi.fn();
    try {
      await expect(
        main(
          [
            '--modules-json',
            JSON.stringify({ bidder: ['rubiconBidAdapter'], userId: [], analytics: [] }),
            '--out',
            path.join(temp, 'out'),
          ],
          {
            packageDir,
            metadataDir,
            resolveSpecifier: () => target,
            createGeneratedPaths,
            buildBundle,
          }
        )
      ).rejects.toThrow('must be a directory contained by the pinned package root');
      expect(createGeneratedPaths).not.toHaveBeenCalled();
      expect(buildBundle).not.toHaveBeenCalled();
    } finally {
      fs.rmSync(temp, { recursive: true, force: true });
    }
  });

  it('fails an escaping package export before creating generated paths or invoking Vite', async () => {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-escape-'));
    const escapedTarget = path.join(temp, 'escaped.js');
    fs.writeFileSync(escapedTarget, 'export {};\n');
    const createGeneratedPaths = vi.fn();
    const buildBundle = vi.fn();
    try {
      await expect(
        main(
          [
            '--modules-json',
            JSON.stringify({ bidder: ['rubiconBidAdapter'], userId: [], analytics: [] }),
            '--out',
            path.join(temp, 'out'),
          ],
          {
            resolveSpecifier: () => escapedTarget,
            createGeneratedPaths,
            buildBundle,
          }
        )
      ).rejects.toThrow('resolves outside the pinned prebid.js package');
      expect(createGeneratedPaths).not.toHaveBeenCalled();
      expect(buildBundle).not.toHaveBeenCalled();
    } finally {
      fs.rmSync(temp, { recursive: true, force: true });
    }
  });

  it.each(['liveIntentShim', 'liveIntentStandard', 'prebidGlobal'])(
    'fails an invalid %s target before creating generated paths or invoking Vite',
    async (targetName) => {
      const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-liveintent-'));
      const createGeneratedPaths = vi.fn();
      const buildBundle = vi.fn();
      try {
        await expect(
          main(
            [
              '--modules-json',
              JSON.stringify({
                bidder: ['rubiconBidAdapter'],
                userId: ['liveIntentIdSystem'],
                analytics: [],
              }),
              '--out',
              path.join(temp, 'out'),
            ],
            {
              [targetName]: path.join(temp, 'missing.js'),
              resolveSpecifier: (specifier) => require.resolve(specifier),
              createGeneratedPaths,
              buildBundle,
            }
          )
        ).rejects.toThrow('could not be resolved');
        expect(createGeneratedPaths).not.toHaveBeenCalled();
        expect(buildBundle).not.toHaveBeenCalled();
      } finally {
        fs.rmSync(temp, { recursive: true, force: true });
      }
    }
  );

  it('fails unsupported analytics before creating generated paths or invoking Vite', async () => {
    const outputDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-fail-'));
    const createGeneratedPaths = vi.fn();
    const buildBundle = vi.fn();
    try {
      let error;
      try {
        await main(
          [
            '--modules-json',
            JSON.stringify({
              bidder: ['rubiconBidAdapter'],
              userId: [],
              analytics: ['mavenDistributionAnalyticsAdapter'],
            }),
            '--out',
            outputDirectory,
          ],
          { createGeneratedPaths, buildBundle }
        );
      } catch (cause) {
        error = String(cause);
      }
      expect(error).toContain(
        'integrations.prebid.bundle.modules.analytics requested "mavenDistributionAnalyticsAdapter"'
      );
      expect(error).toContain('prebid.js 10.26.0');
      expect(error).toContain('modules/mavenDistributionAnalyticsAdapter.js');
      expect(error).toContain('Choose an analytics module shipped by the pinned prebid.js package');
      expect(createGeneratedPaths).not.toHaveBeenCalled();
      expect(buildBundle).not.toHaveBeenCalled();
      expect(fs.existsSync(path.join(outputDirectory, 'manifest.json'))).toBe(false);
    } finally {
      fs.rmSync(outputDirectory, { recursive: true, force: true });
    }
  });

  it('cleans generated source after a build failure', async () => {
    const outputDirectory = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-fail-'));
    const temporaryDir = fs.mkdtempSync(path.join(os.tmpdir(), 'trusted-server-prebid-generated-'));
    const generatedPaths = {
      temporaryDir,
      modulesFile: path.join(temporaryDir, '_modules.generated.ts'),
      entryFile: path.join(temporaryDir, '_external_entry.generated.ts'),
    };
    try {
      await expect(
        main(
          [
            '--modules-json',
            JSON.stringify({ bidder: ['rubiconBidAdapter'], userId: [], analytics: [] }),
            '--out',
            outputDirectory,
          ],
          {
            createGeneratedPaths: () => generatedPaths,
            buildBundle: async () => {
              throw new Error('example Vite failure');
            },
          }
        )
      ).rejects.toThrow('example Vite failure');
      expect(fs.existsSync(temporaryDir)).toBe(false);
      expect(fs.existsSync(path.join(outputDirectory, 'manifest.json'))).toBe(false);
    } finally {
      fs.rmSync(outputDirectory, { recursive: true, force: true });
      fs.rmSync(temporaryDir, { recursive: true, force: true });
    }
  });

  it('writes schema version 1 with effective module and runtime-code lists', async () => {
    const outputDirectory = fs.mkdtempSync(
      path.join(os.tmpdir(), 'trusted-server-prebid-build-test-')
    );

    try {
      await main([
        '--modules-json',
        JSON.stringify({
          bidder: ['rubiconBidAdapter'],
          userId: ['pairIdSystem', 'sharedIdSystem'],
          analytics: ['atsAnalyticsAdapter'],
        }),
        '--out',
        outputDirectory,
      ]);

      const manifest = JSON.parse(
        fs.readFileSync(path.join(outputDirectory, 'manifest.json'), 'utf8')
      );
      const bundle = fs.readFileSync(path.join(outputDirectory, manifest.filename), 'utf8');

      expect(manifest).toMatchObject({
        schemaVersion: 1,
        prebidVersion: '10.26.0',
        modules: {
          bidder: ['rubiconBidAdapter'],
          userId: ['pairIdSystem', 'sharedIdSystem'],
          analytics: ['atsAnalyticsAdapter'],
        },
        runtimeCodes: {
          bidder: ['rubicon'],
          analytics: ['atsAnalytics'],
        },
      });
      expect(manifest).not.toHaveProperty('adapters');
      expect(manifest).not.toHaveProperty('bidderCodes');
      expect(manifest).not.toHaveProperty('userIdModules');
      expect(bundle).toContain('atsAnalyticsAdapter');
    } finally {
      fs.rmSync(outputDirectory, { recursive: true, force: true });
    }
  }, 120_000);
});
