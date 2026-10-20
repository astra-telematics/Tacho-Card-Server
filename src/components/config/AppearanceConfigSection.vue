<template>
    <q-card class="min-width: 350px; width: 75vw">
        <q-card-section>
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
                v-if="isDesktop"
                v-model="minimizeToTrayValue"
                label="Minimise to tray when closed"
                class="q-mt-sm"
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
import { applyTheme, getConfig } from 'src/utils'

const selectedTheme = ref('')
const minimizeToTrayValue = ref(true)
const savedSelectedTheme = ref('')
const savedMinimizeToTray = ref(true)

const isValid = computed(
    () =>
        selectedTheme.value !== '' &&
        (selectedTheme.value !== savedSelectedTheme.value ||
            minimizeToTrayValue.value !== savedMinimizeToTray.value),
)

// Previews the selected theme straight away; Cancel reverts it.
const changeTheme = (value: string) => applyTheme(value)

const saveConfig = async () => {
    try {
        const saved = await invoke<boolean>('update_appearance', {
            theme: selectedTheme.value,
            minimizeToTray: minimizeToTrayValue.value,
        })
        // The backend reports failure (e.g. config file not writable) as `false`.
        if (!saved) throw new Error('update_appearance returned false')

        Notify.create({
            message: 'Appearance configuration has been updated.',
            color: 'green',
            position: 'bottom',
            timeout: 3000,
        })
        savedSelectedTheme.value = selectedTheme.value
        savedMinimizeToTray.value = minimizeToTrayValue.value
    } catch (error) {
        console.log('Error updating appearance configuration:', error)
        Notify.create({
            message: 'Error updating appearance configuration.',
            color: 'red',
            position: 'bottom',
            timeout: 3000,
        })
    }
}

const cancelConfig = () => {
    selectedTheme.value = savedSelectedTheme.value
    minimizeToTrayValue.value = savedMinimizeToTray.value
    applyTheme(savedSelectedTheme.value) // undo any preview
}

// Load the current settings when the tab is created; the dialog's contents only exist
// while it's open, so the startup `global-config-appearance` event has long gone.
onMounted(async () => {
    try {
        const config = await getConfig()
        selectedTheme.value = config.dark_theme
        savedSelectedTheme.value = config.dark_theme
        minimizeToTrayValue.value = config.minimize_to_tray !== 'false'
        savedMinimizeToTray.value = config.minimize_to_tray !== 'false'
    } catch (error) {
        console.error('Error loading appearance configuration:', error)
    }
})
</script>
