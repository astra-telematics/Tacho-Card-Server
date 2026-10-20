<template>
    <q-card>
        <q-card-section>
            <q-input
                label="Log Retention Days"
                dense
                type="number"
                v-model.number="logRetentionDaysValue"
                :min="1"
                :max="MAX_DAYS"
                :rules="[inRange(1, MAX_DAYS)]"
                @keyup.enter="saveConfig"
            />
            <q-btn
                v-if="isDesktop"
                flat
                label="Open Logs Folder"
                color="primary"
                class="q-mt-md"
                @click="openLogsFolder"
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
import { invoke, isDesktop } from 'src/backend'
import { getConfig, inRange, isIntInRange } from 'src/utils'

// Upper bound for the day-count fields: ten years is far beyond any sensible value.
const MAX_DAYS = 3650

const logRetentionDaysValue = ref<number>(60)
const savedLogRetentionDays = ref(60)

// True when the field is valid and has changed
const isValid = computed(
    () =>
        isIntInRange(logRetentionDaysValue.value, 1, MAX_DAYS) &&
        logRetentionDaysValue.value !== savedLogRetentionDays.value,
)

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

const saveConfig = async () => {
    try {
        // Tauri maps camelCase keys to the Rust command's snake_case parameters.
        const saved = await invoke<boolean>('update_logging', {
            logRetentionDays: logRetentionDaysValue.value.toString(),
        })
        // The backend reports failure (e.g. config file not writable) as `false`.
        if (!saved) throw new Error('update_logging returned false')

        Notify.create({
            message: 'Logging configuration has been updated.',
            color: 'green',
            position: 'bottom',
            timeout: 3000,
        })
        savedLogRetentionDays.value = logRetentionDaysValue.value
    } catch (error) {
        console.log('Error updating logging configuration:', error)
        Notify.create({
            message: 'Error updating logging configuration.',
            color: 'red',
            position: 'bottom',
            timeout: 3000,
        })
    }
}

const cancelConfig = () => {
    logRetentionDaysValue.value = savedLogRetentionDays.value
}

// Load the current settings when the tab is created; the dialog's contents only exist
// while it's open, so the startup `global-config-logging` event has long gone.
onMounted(async () => {
    try {
        const config = await getConfig()
        logRetentionDaysValue.value = Number(config.retention_days)
        savedLogRetentionDays.value = Number(config.retention_days)
    } catch (error) {
        console.error('Error loading logging configuration:', error)
    }
})
</script>
