<template>
    <q-card>
        <q-card-section>
            <q-input
                label="Download Path"
                :hint="
                    downloadPathEnv
                        ? `Set by the ${downloadPathEnv} environment variable. Each company's downloads are saved in a subfolder named after its id.`
                        : `Each company's downloads are saved in a subfolder named after its id.`
                "
                dense
                v-model="downloadPathValue"
                :readonly="!!downloadPathEnv"
                @keyup.enter="saveConfig"
            >
                <template v-if="downloadPathEnv" #prepend><q-icon name="mdi-lock-outline" /></template>
                <template v-if="isDesktop && !downloadPathEnv" #append>
                    <q-btn flat round dense icon="mdi-folder-open" @click="browseDownloadPath">
                        <q-tooltip>Browse…</q-tooltip>
                    </q-btn>
                </template>
            </q-input>
            <q-checkbox
                label="Retain failed and partial downloads"
                dense
                v-model="retainFailedDownloadsValue"
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
import { invoke, isDesktop } from 'src/backend'
import { open } from '@tauri-apps/plugin-dialog'
import { getConfig } from 'src/utils'

const downloadPathValue = ref('')
const retainFailedDownloadsValue = ref(false)
const savedDownloadPath = ref('')
const savedRetainFailedDownloads = ref(false)
// Set when the folder is pinned by an environment variable (e.g. in Docker).
const downloadPathEnv = ref('')

// The download interval is per company (Companies tab), not here.
const isValid = computed(
    () =>
        downloadPathValue.value !== savedDownloadPath.value ||
        retainFailedDownloadsValue.value !== savedRetainFailedDownloads.value,
)

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

const saveConfig = async () => {
    try {
        // Tauri maps camelCase keys to the Rust command's snake_case parameters.
        const saved = await invoke<boolean>('update_download', {
            downloadPath: downloadPathValue.value,
            retainFailedDownloads: retainFailedDownloadsValue.value,
        })
        // The backend reports failure (e.g. config file not writable) as `false`.
        if (!saved) throw new Error('update_download returned false')

        Notify.create({
            message: 'Download configuration has been updated.',
            color: 'green',
            position: 'bottom',
            timeout: 3000,
        })
        savedDownloadPath.value = downloadPathValue.value
        savedRetainFailedDownloads.value = retainFailedDownloadsValue.value
    } catch (error) {
        console.log('Error updating download configuration:', error)
        Notify.create({
            message: 'Error updating download configuration.',
            color: 'red',
            position: 'bottom',
            timeout: 3000,
        })
    }
}

const cancelConfig = () => {
    downloadPathValue.value = savedDownloadPath.value
    retainFailedDownloadsValue.value = savedRetainFailedDownloads.value
}

// Load the current settings when the tab is created. The startup
// `global-config-download` event has long gone by then: the dialog's contents only
// exist while it's open.
onMounted(async () => {
    try {
        const config = await getConfig()
        downloadPathValue.value = config.download_path
        savedDownloadPath.value = config.download_path
        retainFailedDownloadsValue.value = config.retain_failed_downloads === 'true'
        savedRetainFailedDownloads.value = config.retain_failed_downloads === 'true'
        downloadPathEnv.value = config.download_path_env
    } catch (error) {
        console.error('Error loading download configuration:', error)
    }
})
</script>
