<template>
    <q-card>
        <q-card-section>
            <div class="text-h6">Connection</div>
            <q-input
                label="Listen Port"
                dense
                type="number"
                v-model.number="listenPortValue"
                :min="1"
                :max="65535"
                :rules="[inRange(1, 65535)]"
                @keyup.enter="saveConfig"
            />
            <q-input
                label="Connection Limit"
                dense
                type="number"
                v-model.number="connectionLimitValue"
                :min="1"
                :max="65535"
                :rules="[inRange(1, 65535)]"
                @keyup.enter="saveConfig"
            />
        </q-card-section>
        <q-card-actions align="right" class="text-primary">
            <q-btn flat label="Cancel" @click="cancelConfig" />
            <q-btn
                unelevated
                label="Save"
                :color="isValid ? 'positive' : 'grey'"
                :disable="!isValid"
                @click="saveConfig"
            />
        </q-card-actions>
    </q-card>
</template>

<script setup lang="ts">
import { Notify } from 'quasar'
import { computed, onMounted, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { getConfig, inRange, isIntInRange } from 'src/utils'

const listenPortValue = ref<number>(7878)
const connectionLimitValue = ref<number>(100)
const savedListenPort = ref(7878)
const savedConnectionLimit = ref(100)

// True when every field is valid and something has changed
const isValid = computed(
    () =>
        isIntInRange(listenPortValue.value, 1, 65535) &&
        isIntInRange(connectionLimitValue.value, 1, 65535) &&
        (listenPortValue.value !== savedListenPort.value ||
            connectionLimitValue.value !== savedConnectionLimit.value),
)

const saveConfig = async () => {
    try {
        // Tauri maps camelCase keys to the Rust command's snake_case parameters.
        const saved = await invoke<boolean>('update_connection', {
            listenPort: listenPortValue.value.toString(),
            connectionLimit: connectionLimitValue.value.toString(),
        })
        // The backend reports failure (e.g. config file not writable) as `false`.
        if (!saved) throw new Error('update_connection returned false')

        Notify.create({
            message: 'Connection configuration has been updated.',
            color: 'green',
            position: 'bottom',
            timeout: 3000,
        })
        savedListenPort.value = listenPortValue.value
        savedConnectionLimit.value = connectionLimitValue.value
    } catch (error) {
        console.log('Error updating connection configuration:', error)
        Notify.create({
            message: 'Error updating connection configuration.',
            color: 'red',
            position: 'bottom',
            timeout: 3000,
        })
    }
}

const cancelConfig = () => {
    listenPortValue.value = savedListenPort.value
    connectionLimitValue.value = savedConnectionLimit.value
}

// Load the current settings when the tab is created; the dialog's contents only exist
// while it's open, so the startup `global-config-connection` event has long gone.
onMounted(async () => {
    try {
        const config = await getConfig()
        listenPortValue.value = Number(config.listen_port)
        savedListenPort.value = Number(config.listen_port)
        connectionLimitValue.value = Number(config.connection_limit)
        savedConnectionLimit.value = Number(config.connection_limit)
    } catch (error) {
        console.error('Error loading connection configuration:', error)
    }
})
</script>
