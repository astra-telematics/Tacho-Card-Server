import { Dark } from 'quasar'
import { invoke } from 'src/backend'

export const isIntInRange = (value: number | string, min: number, max: number) =>
  typeof value === 'number' && Number.isInteger(value) && value >= min && value <= max

// Quasar rule: true when valid, otherwise the message shown under the field.
export const inRange = (min: number, max: number) => (value: number | string) =>
  isIntInRange(value, min, max) || `Enter a whole number from ${min} to ${max}`

// Settings as sent by the backend: the `get_config` command, and the `global-config-*`
// events (each with its own subset). All values are strings (see config.rs::get_from_cache).
export interface AppConfig {
  listen_port: string
  connection_limit: string
  download_path: string
  retain_failed_downloads: string
  retention_days: string
  dark_theme: string
  minimize_to_tray: string
}

// Current settings. Call when a config section is created: the dialog's contents only
// exist while it's open, so they miss the startup `global-config-*` events.
export const getConfig = () => invoke<AppConfig>('get_config')

// Applies a saved theme value ('Light' | 'Auto' | 'Dark').
export const applyTheme = (value: string) => {
  switch (value) {
    case 'Auto':
      Dark.set('auto')
      break
    case 'Dark':
      Dark.set(true)
      break
    case 'Light':
      Dark.set(false)
      break
    default:
      console.log('Unknown theme value:', value)
  }
}
