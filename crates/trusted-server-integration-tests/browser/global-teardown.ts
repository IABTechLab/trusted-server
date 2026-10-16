import { readFileSync, unlinkSync } from 'node:fs'
import { resolve } from 'node:path'
import { stopContainer, stopViceroy } from './helpers/infra.js'

const STATE_FILE = resolve(__dirname, '.browser-test-state.json')

async function globalTeardown(): Promise<void> {
  let state: {
    containerId?: string
    viceroyPid?: number
    extraViceroyPids?: number[]
  }
  try {
    state = JSON.parse(readFileSync(STATE_FILE, 'utf-8'))
  } catch {
    console.warn('[global-teardown] No state file found, nothing to clean up')
    return
  }

  let cleanupError: unknown
  const pids = new Set([state.viceroyPid, ...(state.extraViceroyPids ?? [])])
  for (const pid of pids) {
    if (pid === undefined) continue
    console.log(`[global-teardown] Stopping Viceroy (pid: ${pid})`)
    try {
      await stopViceroy(pid)
    } catch (error) {
      cleanupError ??= error
    }
  }

  if (state.containerId) {
    console.log(
      `[global-teardown] Stopping container ${state.containerId.slice(0, 12)}...`
    )
    stopContainer(state.containerId)
  }

  try {
    unlinkSync(STATE_FILE)
  } catch {
    // Already removed
  }
  if (cleanupError) throw cleanupError
}

export default globalTeardown
