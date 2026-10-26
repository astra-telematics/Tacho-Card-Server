import { Dark } from 'quasar'
import { invoke } from 'src/backend'

export const isIntInRange = (value: number | string, min: number, max: number) =>
  typeof value === 'number' && Number.isInteger(value) && value >= min && value <= max

// Quasar rule: true when valid, otherwise the message shown under the field.
export const inRange = (min: number, max: number) => (value: number | string) =>
  isIntInRange(value, min, max) || `Enter a whole number from ${min} to ${max}`

// Dates are shown in the viewer's own format. A browser's default locale is its UI
// language, often en-US wherever the viewer is; `navigator.languages` is the languages
// they've chosen in its settings (e.g. en-GB), so it's used instead.
const viewerLocales = [...navigator.languages]

// Date and time, in the viewer's time zone.
export const formatDateTime = (unixSeconds: number) =>
  new Date(unixSeconds * 1000).toLocaleString(viewerLocales)

// Date only. `utc` for a date stored as midnight UTC (a card's expiry), which would
// otherwise show as the previous day west of Greenwich.
export const formatDate = (unixSeconds: number, utc = false) =>
  new Date(unixSeconds * 1000).toLocaleDateString(viewerLocales, utc ? { timeZone: 'UTC' } : {})

// Settings as sent by the backend: the `get_config` command, and the `global-config-*`
// events (each with its own subset). All values are strings (see config.rs::get_from_cache).
export interface AppConfig {
  listen_port: string
  connection_limit: string
  download_path: string
  retain_failed_downloads: string
  retention_days: string
  minimize_to_tray: string
  // Name of the environment variable pinning the setting (read-only in the UI), or ''.
  listen_port_env: string
  download_path_env: string
}

// Current settings. Call when a config section is created: the dialog's contents only
// exist while it's open, so they miss the startup `global-config-*` events.
export const getConfig = () => invoke<AppConfig>('get_config')

// The theme is per user (the desktop app uses admin's), stored by the backend.
export const THEME_OPTIONS = [
  { icon: 'mdi-white-balance-sunny', value: 'Light', label: 'Light' },
  { icon: 'mdi-brightness-auto', value: 'Auto', label: 'Auto' },
  { icon: 'mdi-weather-night', value: 'Dark', label: 'Dark' },
]

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
