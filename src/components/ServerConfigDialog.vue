<template>
  <q-dialog
    :model-value="modelValue"
    persistent
    @update:model-value="$emit('update:modelValue', $event)"
  >
    <q-card style="min-width: 350px; width: 75vw">
      <q-card-section>
        <div class="text-h6">Server</div>
        <q-input
          label="Listen Port"
          dense
          type="number"
          v-model.number="listenPortValue"
          :min="1"
          :max="65535"
          :rules="[inRange(1, 65535)]"
          @keyup.enter="saveAndClose"
        />
        <q-input
          label="Connection Limit"
          dense
          type="number"
          v-model.number="connectionLimitValue"
          :min="1"
          :max="65535"
          :rules="[inRange(1, 65535)]"
          @keyup.enter="saveAndClose"
        />
        <q-input
          label="Download Interval (days)"
          dense
          type="number"
          v-model.number="downloadIntervalDaysValue"
          :min="0"
          :max="MAX_DOWNLOAD_INTERVAL_DAYS"
          :rules="[inRange(0, MAX_DOWNLOAD_INTERVAL_DAYS)]"
          hint="Minimum days between downloads per device (max 60). 0 = no limit."
          @keyup.enter="saveAndClose"
        />
        <q-input
          label="Download Path"
          dense
          v-model="downloadPathValue"
          @keyup.enter="saveAndClose"
        >
          <template #append>
            <q-btn flat round dense icon="mdi-folder-open" @click="browseDownloadPath">
              <q-tooltip>Browse…</q-tooltip>
            </q-btn>
          </template>
        </q-input>
      </q-card-section>

      <q-card-section class="q-pt-none">
        <div class="text-h6">Logging</div>
        <q-input
          label="Log Retention Days"
          dense
          type="number"
          v-model.number="logRetentionDaysValue"
          :min="1"
          :max="MAX_DAYS"
          :rules="[inRange(1, MAX_DAYS)]"
          @keyup.enter="saveAndClose"
        />
        <q-btn
          flat
          label="Open Logs Folder"
          color="primary"
          class="q-mt-md"
          @click="openLogsFolder"
        />
      </q-card-section>

      <q-card-section class="q-pt-none">
        <div class="text-h6">Appearance</div>
        <div class="q-mt-md row items-center">
          <div class="text-caption text-grey q-mr-md">Theme</div>
          <q-btn-toggle
            v-model="selectedTheme"
            no-caps
            rounded
            unelevated
            toggle-color="primary"
            :options="[
              { icon: 'mdi-white-balance-sunny', value: 'Light' },
              { icon: 'mdi-brightness-auto', value: 'Auto' },
              { icon: 'mdi-weather-night', value: 'Dark' },
            ]"
            @update:model-value="changeTheme"
          />
        </div>
        <q-toggle
          v-model="minimizeToTrayValue"
          label="Minimise to tray when closed"
          class="q-mt-sm"
        />
      </q-card-section>
      <q-card-actions align="right" class="text-primary">
        <q-btn flat label="Cancel" v-close-popup />
        <q-btn flat label="Save" v-close-popup :disable="!isValid" @click="saveServerConfig" />
      </q-card-actions>
    </q-card>
  </q-dialog>
</template>

<script setup lang="ts">
import { useQuasar, Notify } from 'quasar'
import { computed, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { open } from '@tauri-apps/plugin-dialog'

defineProps<{
  modelValue: boolean
}>()

const emit = defineEmits<{
  'update:modelValue': [value: boolean]
}>()

// v-model.number yields '' while a number field is empty, hence `number | string`.
const listenPortValue = ref<number | string>(7878)
const connectionLimitValue = ref<number | string>(100)
const downloadIntervalDaysValue = ref<number | string>(1)
const logRetentionDaysValue = ref<number | string>(7)
const downloadPathValue = ref('')
const minimizeToTrayValue = ref(true)

// Upper bound for the day-count fields: ten years is far beyond any sensible value.
const MAX_DAYS = 3650
// Regulations require a VU download at least every 90 days; matches
// MAX_DOWNLOAD_INTERVAL_DAYS in config.rs.
const MAX_DOWNLOAD_INTERVAL_DAYS = 60

const isIntInRange = (value: number | string, min: number, max: number) =>
  typeof value === 'number' && Number.isInteger(value) && value >= min && value <= max

// Quasar rule: true when valid, otherwise the message shown under the field.
const inRange = (min: number, max: number) => (value: number | string) =>
  isIntInRange(value, min, max) || `Enter a whole number from ${min} to ${max}`

const isValid = computed(
  () =>
    isIntInRange(listenPortValue.value, 1, 65535) &&
    isIntInRange(connectionLimitValue.value, 1, 65535) &&
    isIntInRange(downloadIntervalDaysValue.value, 0, MAX_DOWNLOAD_INTERVAL_DAYS) &&
    isIntInRange(logRetentionDaysValue.value, 1, MAX_DAYS),
)

const $q = useQuasar()
const selectedTheme = ref('')

const changeTheme = (value: string) => {
  switch (value) {
    case 'Auto':
      $q.dark.set('auto')
      break
    case 'Dark':
      $q.dark.set(true)
      break
    case 'Light':
      $q.dark.set(false)
      break
    default:
      console.log('Unknown theme value:', value)
  }
}

// Enter in any field: same as clicking Save, which is disabled while a field is invalid.
const saveAndClose = async () => {
  if (!isValid.value) return
  emit('update:modelValue', false)
  await saveServerConfig()
}

const saveServerConfig = async () => {
  console.log(`listen_port: ${listenPortValue.value}, theme: ${selectedTheme.value}`)

  try {
    const response = await invoke('update_server', {
      listenPort: listenPortValue.value.toString(),
      connectionLimit: connectionLimitValue.value.toString(),
      downloadPath: downloadPathValue.value,
      downloadIntervalDays: downloadIntervalDaysValue.value.toString(),
      logRetentionDays: logRetentionDaysValue.value.toString(),
      theme: selectedTheme.value,
      minimizeToTray: minimizeToTrayValue.value,
    })

    console.log('Response from update_server:', response)

    Notify.create({
      message: 'Server configuration has been updated.',
      color: 'green',
      position: 'bottom',
      timeout: 3000,
    })

    await invoke('manual_sync_cards', {
      readername: '',
      restart: true,
    })
  } catch (error) {
    console.log('Error updating server configuration:', error)
    Notify.create({
      message: 'Failed to update server configuration.',
      color: 'red',
      position: 'bottom',
      timeout: 3000,
    })
  }
}

// Listen for config updates from backend
listen('global-config-server', (event) => {
  // All values arrive as strings (see config.rs::get_from_cache).
  const payload = event.payload as {
    listen_port: string
    connection_limit: string
    download_path: string
    download_interval_days: string
    retention_days: string
    dark_theme: string
    minimize_to_tray: string
  }
  console.log(
    'listen_port:',
    payload.listen_port,
    'connection_limit:',
    payload.connection_limit,
    'dark_theme:',
    payload.dark_theme,
  )

  listenPortValue.value = Number(payload.listen_port)
  connectionLimitValue.value = Number(payload.connection_limit)
  downloadPathValue.value = payload.download_path
  downloadIntervalDaysValue.value = Number(payload.download_interval_days)
  logRetentionDaysValue.value = Number(payload.retention_days)
  // Backend cache values are plain strings; empty (no appearance section yet) means on.
  minimizeToTrayValue.value = payload.minimize_to_tray !== 'false'

  changeTheme(payload.dark_theme)
  selectedTheme.value = payload.dark_theme
}).catch((error) => {
  console.error('Error listening to global-config-server:', error)
})

const browseDownloadPath = async () => {
  try {
    const selected = await open({
      directory: true,
      multiple: false,
      title: 'Select download folder',
      ...(downloadPathValue.value ? { defaultPath: downloadPathValue.value } : {}),
    })
    // null when the picker is cancelled — keep the current path.
    if (typeof selected === 'string') {
      downloadPathValue.value = selected
    }
  } catch (error) {
    console.log('Error opening folder picker:', error)
    Notify.create({
      message: 'Failed to open the folder picker.',
      color: 'red',
      position: 'bottom',
      timeout: 3000,
    })
  }
}

const openLogsFolder = async () => {
  try {
    await invoke('open_logs_folder')
  } catch (error) {
    console.log('Error opening logs folder:', error)
    Notify.create({
      message: 'Failed to open the logs folder.',
      color: 'red',
      position: 'bottom',
      timeout: 3000,
    })
  }
}
</script>
