/**
 * Acquire ordinary CI Linux CLI fixture images.
 *
 * Builds the glibc 2.34 builder image and pulls the clean startup images,
 * recording each by its local image ID. This is unauthenticated ordinary CI
 * acquisition: tags are not identities and nothing here qualifies a release
 * tool, image or input.
 */
import fs from 'node:fs'
import path from 'node:path'
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const root = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../..'
)
const DOCKERFILE = 'scripts/release/tests/cli-builder.Dockerfile'
// Must match `StartupDistribution::reference` in the release producer.
const STARTUP_IMAGES = [
  ['ubi9', 'registry.access.redhat.com/ubi9/ubi-minimal:9.6'],
  ['ubuntu-22.04', 'docker.io/library/ubuntu:22.04'],
  ['debian-12', 'docker.io/library/debian:12'],
]
const IMAGE_ID = /^sha256:[a-f0-9]{64}$/

const [destinationArgument] = process.argv.slice(2)
if (process.platform !== 'linux') throw new Error('CLI fixture requires Linux')
if (!destinationArgument || process.argv.length !== 3)
  throw new Error('Expected one fresh fixture destination')
const destination = path.resolve(destinationArgument)
if (fs.existsSync(destination))
  throw new Error('Fixture destination must be fresh')
fs.mkdirSync(destination, { recursive: true, mode: 0o700 })
const dockerConfig = path.join(destination, 'docker-config')
fs.mkdirSync(dockerConfig, { mode: 0o700 })

const run = (program, args) =>
  execFileSync(program, args, {
    cwd: root,
    env: {
      PATH: '/usr/bin:/bin',
      GIT_CONFIG_NOSYSTEM: '1',
      GIT_CONFIG_GLOBAL: '/dev/null',
    },
    timeout: 20 * 60_000,
    maxBuffer: 64 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'inherit'],
  })
    .toString('utf8')
    .trim()
const docker = (args) =>
  run('/usr/bin/docker', [
    '--config',
    dockerConfig,
    '--host',
    'unix:///var/run/docker.sock',
    ...args,
  ])
const imageId = (text, label) => {
  if (!IMAGE_ID.test(text))
    throw new Error(`Expected a local image ID for ${label}`)
  return text
}
const requireNoVolumes = (id) => {
  const volumes = JSON.parse(
    docker(['image', 'inspect', '--format', '{{json .Config.Volumes}}', id])
  )
  if (
    volumes !== null &&
    (!volumes ||
      typeof volumes !== 'object' ||
      Array.isArray(volumes) ||
      Object.keys(volumes).length !== 0)
  )
    throw new Error(`Fixture image declares implicit writable volumes: ${id}`)
}

// 1. The tracked builder Dockerfile, unmodified from HEAD.
run('git', ['diff', '--quiet', 'HEAD', '--', DOCKERFILE])
const imageFile = path.join(destination, 'builder-image-id')
docker([
  'build',
  '--pull',
  '--iidfile',
  imageFile,
  '--file',
  path.join(root, DOCKERFILE),
  path.join(root, 'scripts/release/tests'),
])
const builderImage = imageId(
  fs.readFileSync(imageFile, 'utf8').trim(),
  'the builder'
)
requireNoVolumes(builderImage)
const glibc = docker([
  'run',
  '--rm',
  '--network=none',
  builderImage,
  'ldd',
  '--version',
])
if (!/ 2\.34$/m.test(glibc.split('\n')[0] ?? ''))
  throw new Error(`Builder glibc is not 2.34: ${glibc.split('\n')[0]}`)

// 2. Clean startup images, selected later only by local image ID.
const startupImages = STARTUP_IMAGES.map(([distribution, reference]) => {
  docker(['pull', '--quiet', reference])
  const id = docker(['image', 'inspect', '--format', '{{.Id}}', reference])
  const selected = imageId(id, reference)
  requireNoVolumes(selected)
  return { distribution, reference, image_id: selected }
})

fs.rmSync(dockerConfig, { recursive: true, force: true })
fs.writeFileSync(
  path.join(destination, 'cli-fixture.json'),
  JSON.stringify({
    schema_version: 1,
    kind: 'ordinary-ci-cli-fixture',
    acquisition: 'unauthenticated-ordinary-ci',
    builder_image_id: builderImage,
    builder_glibc: glibc.split('\n')[0],
    startup_images: startupImages,
  }) + '\n',
  { flag: 'wx', mode: 0o600 }
)
process.stdout.write(
  `Prepared the glibc 2.34 builder ${builderImage} and ` +
    `${startupImages.length} startup images by unauthenticated ordinary ` +
    'CI acquisition; this is not release qualification\n'
)
