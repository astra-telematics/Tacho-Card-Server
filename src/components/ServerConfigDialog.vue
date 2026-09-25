<template>
  <q-dialog
    :model-value="modelValue"
    persistent
    @update:model-value="$emit('update:modelValue', $event)"
  >
    <q-card style="min-width: 350px">
      <q-card-section>
        <div class="text-h6">Server configuration</div>
      </q-card-section>

      <q-card-section class="q-pt-none">
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
          v-model="allowAnonymousData"
          label="Allow anonymous data collection"
        />
        <div>
          <q-btn
            flat
            dense
            no-caps
            color="primary"
            label="Open collected data"
            @click="openTelemetryLog"
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

defineProps<{
  modelValue: boolean
}>()

defineEmits<{
  'update:modelValue': [value: boolean]
}>()

const listenPortValue = ref(7878)
const connectionLimitValue = ref(1000)
const allowAnonymousData = ref(true)

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
      listenPort: listenPortValue.value,
      connectionLimit: connectionLimitValue.value,
      theme: selectedTheme.value,
      allowAnonymousData: allowAnonymousData.value,
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
    dark_theme: string
    allow_anonymous_data: string
  }
  console.log(
    'listen_port:',
    payload.listen_port,
    'connection_limit:',
    payload.connection_limit,
    'dark_theme:',
    payload.dark_theme,
    'allow_anonymous_data:',
    payload.allow_anonymous_data,
  )

  listenPortValue.value = payload.listen_port
  connectionLimitValue.value = payload.connection_limit

  changeTheme(payload.dark_theme)
  selectedTheme.value = payload.dark_theme

  // Backend cache values are all plain strings (see config.rs::get_from_cache).
  allowAnonymousData.value = payload.allow_anonymous_data === 'true'
}).catch((error) => {
  console.error('Error listening to global-config-server:', error)
})

const openTelemetryLog = async () => {
  try {
    await invoke('open_telemetry_log')
  } catch (error) {
    console.log('Error opening telemetry log:', error)
    Notify.create({
      message: 'Failed to open the collected data file.',
      color: 'red',
      position: 'bottom',
      timeout: 3000,
    })
  }
}
</script>
