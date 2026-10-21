<template>
    <!-- Desktop only: the window's behaviour. The theme is per user, in the account menu. -->
    <q-card class="min-width: 350px; width: 75vw">
        <q-card-section>
            <q-toggle v-model="minimizeToTrayValue" label="Minimise to tray when closed" />
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
import { invoke } from 'src/backend'
import { getConfig } from 'src/utils'

const minimizeToTrayValue = ref(true)
const savedMinimizeToTray = ref(true)

const isValid = computed(() => minimizeToTrayValue.value !== savedMinimizeToTray.value)

const saveConfig = async () => {
    try {
        const saved = await invoke<boolean>('update_appearance', {
            minimizeToTray: minimizeToTrayValue.value,
        })
        // The backend reports failure (e.g. config file not writable) as `false`.
        if (!saved) throw new Error('update_appearance returned false')

        Notify.create({
            message: 'Window configuration has been updated.',
            color: 'green',
            position: 'bottom',
            timeout: 3000,
        })
        savedMinimizeToTray.value = minimizeToTrayValue.value
    } catch (error) {
        console.log('Error updating window configuration:', error)
        Notify.create({
            message: 'Error updating window configuration.',
            color: 'red',
            position: 'bottom',
            timeout: 3000,
        })
    }
}

const cancelConfig = () => {
    minimizeToTrayValue.value = savedMinimizeToTray.value
}

// Load the current settings when the tab is created; the dialog's contents only exist
// while it's open, so the startup `global-config-appearance` event has long gone.
onMounted(async () => {
    try {
        const config = await getConfig()
        minimizeToTrayValue.value = config.minimize_to_tray !== 'false'
        savedMinimizeToTray.value = config.minimize_to_tray !== 'false'
    } catch (error) {
        console.error('Error loading window configuration:', error)
    }
})
</script>
