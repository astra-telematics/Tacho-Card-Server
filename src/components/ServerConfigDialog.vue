<template>
  <q-dialog
    :model-value="modelValue"
    persistent
    @update:model-value="$emit('update:modelValue', $event)"
  >
    <q-card style="min-width: 350px">
      <q-card-section>
        <div class="text-h6">Server</div>
        <q-input
          label="Listen Port"
          dense
          v-model="listenPortValue"
          @keyup.enter="$emit('update:modelValue', false)"
        />
        <q-input
          label="Connection Limit"
          dense
          v-model="connectionLimitValue"
          @keyup.enter="$emit('update:modelValue', false)"
        />
        <q-input
          label="Download Path"
          dense
          v-model="downloadPathValue"
          @keyup.enter="$emit('update:modelValue', false)"
        >
          <template #append>
            <q-btn
              flat
              round
              dense
              icon="mdi-folder-open"
              @click="browseDownloadPath"
            >
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
          v-model="logRetentionDaysValue"
          @keyup.enter="$emit('update:modelValue', false)"
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
      </q-card-section>
      <q-card-actions align="right" class="text-primary">
        <q-btn flat label="Cancel" v-close-popup />
        <q-btn flat label="Save" v-close-popup @click="saveServerConfig" />
      </q-card-actions>
    </q-card>
  </q-dialog>
</template>

<script setup lang="ts">
import { useQuasar, Notify } from 'quasar'
import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { open } from '@tauri-apps/plugin-dialog'

defineProps<{
  modelValue: boolean
}>()

defineEmits<{
  'update:modelValue': [value: boolean]
}>()

const listenPortValue = ref(7878)
const connectionLimitValue = ref(100)
const logRetentionDaysValue = ref(7)
const downloadPathValue = ref('')

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

const saveServerConfig = async () => {
  console.log(`listen_port: ${listenPortValue.value}, theme: ${selectedTheme.value}`)

  try {
    const response = await invoke('update_server', {
      listenPort: listenPortValue.value.toString(),
      connectionLimit: connectionLimitValue.value.toString(),
      downloadPath: downloadPathValue.value,
      logRetentionDays: logRetentionDaysValue.value.toString(),
      theme: selectedTheme.value,
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
  const payload = event.payload as {
    listen_port: number
    connection_limit: number
    download_path: string
    retention_days: number
    dark_theme: string
  }
  console.log(
    'listen_port:',
    payload.listen_port,
    'connection_limit:',
    payload.connection_limit,
    'dark_theme:',
    payload.dark_theme,
  )

  listenPortValue.value = payload.listen_port
  connectionLimitValue.value = payload.connection_limit
  downloadPathValue.value = payload.download_path
  logRetentionDaysValue.value = payload.retention_days

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
