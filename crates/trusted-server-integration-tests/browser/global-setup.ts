import { writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import type { TestState } from './helpers/state.js'
import {
  startContainer,
  startViceroy,
  stopContainer,
  stopViceroy,
} from './helpers/infra.js'

const STATE_FILE = resolve(__dirname, '.browser-test-state.json')

const WASM_PATH =
  process.env.WASM_BINARY_PATH ||
  resolve(
    __dirname,
    '../../../target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm'
  )

const VICEROY_CONFIG =
  process.env.VICEROY_CONFIG_PATH ||
  resolve(
    __dirname,
    '../../../target/integration-test-artifacts/configs/viceroy.toml'
  )

/** Persist current state so global-teardown can always clean up. */
function writeState(state: Partial<TestState> & { framework: string }): void {
  writeFileSync(STATE_FILE, JSON.stringify(state, null, 2))
}

async function globalSetup(): Promise<void> {
  const framework = process.env.TEST_FRAMEWORK || 'nextjs'
  let containerId: string | undefined
  let viceroyPid: number | undefined
  const extraViceroyPids: number[] = []

  try {
    console.log(`[global-setup] Starting ${framework} container...`)
    containerId = await startContainer(framework)

    // Write partial state immediately so teardown can stop the container
    // even if Viceroy startup fails below.
    writeState({ containerId, framework })

    console.log(`[global-setup] Starting Viceroy (WASM: ${WASM_PATH})...`)
    const viceroy = await startViceroy(WASM_PATH, VICEROY_CONFIG)
    viceroyPid = viceroy.process.pid

    console.log(`[global-setup] Viceroy ready at ${viceroy.baseUrl}`)

    // Write complete state for tests and teardown
    const state: Partial<TestState> & { framework: string } = {
      baseUrl: viceroy.baseUrl,
      containerId,
      viceroyPid,
      framework,
      extraViceroyPids,
    }
    writeState(state)
    const traceConfigs = [
      [process.env.TRACE_VICEROY_CONFIG_PATH, 'traceBaseUrl'],
      [process.env.TRACE_AUTH_VICEROY_CONFIG_PATH, 'traceAuthBaseUrl'],
    ] as const
    for (const [config, key] of traceConfigs) {
      if (!config) continue
      const fixture = await startViceroy(WASM_PATH, config)
      if (fixture.process.pid !== undefined)
        extraViceroyPids.push(fixture.process.pid)
      state[key] = fixture.baseUrl
      writeState(state)
    }
  } catch (err) {
    // Clean up any resources that were started before re-throwing
    console.error('[global-setup] Setup failed, cleaning up...')
    for (const pid of [viceroyPid, ...extraViceroyPids]) {
      if (pid === undefined) continue
      try {
        await stopViceroy(pid)
      } catch {
        console.warn(`[global-setup] Could not stop owned runtime ${pid}`)
      }
    }
    if (containerId) stopContainer(containerId)

    // Remove partial state file since we cleaned up manually
    try {
      const { unlinkSync } = await import('node:fs')
      unlinkSync(STATE_FILE)
    } catch {
      // State file may not exist
    }

    throw err
  }
}

export default globalSetup
