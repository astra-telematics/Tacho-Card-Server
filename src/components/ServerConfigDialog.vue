<template>
  <q-dialog
    :model-value="modelValue"
    @update:model-value="$emit('update:modelValue', $event)"
  >
    <!-- At least 80% of the viewport each way. max-width overrides QDialog's default
         560px cap; the column layout lets the panels take up the extra height. -->
    <q-card class="column no-wrap" style="min-width: 80vw; max-width: 95vw; min-height: 80vh">
      <q-tabs v-model="tab" dense align="left" active-color="primary" indicator-color="primary">
        <q-tab name="connection" icon="mdi-lan-connect" label="Connection" />
        <q-tab name="download" icon="mdi-download" label="Download" />
        <q-tab name="logging" icon="mdi-text-box-outline" label="Logging" />
        <q-tab name="appearance" icon="mdi-palette" label="Appearance" />
      </q-tabs>

      <q-separator />

      <!-- Each section owns its own state, loading and saving. -->
      <q-tab-panels v-model="tab" animated class="col">
        <q-tab-panel name="connection">
          <ConnectionConfigSection />
        </q-tab-panel>
        <q-tab-panel name="download">
          <DownloadConfigSection />
        </q-tab-panel>
        <q-tab-panel name="logging">
          <LoggingConfigSection />
        </q-tab-panel>
        <q-tab-panel name="appearance">
          <AppearanceConfigSection />
        </q-tab-panel>
      </q-tab-panels>

      <q-card-actions align="right" class="text-primary">
        <q-btn flat label="Close" v-close-popup />
      </q-card-actions>
    </q-card>
  </q-dialog>
</template>

<script setup lang="ts">
import { ref } from 'vue'
import ConnectionConfigSection from 'src/components/config/ConnectionConfigSection.vue'
import DownloadConfigSection from 'src/components/config/DownloadConfigSection.vue'
import LoggingConfigSection from 'src/components/config/LoggingConfigSection.vue'
import AppearanceConfigSection from 'src/components/config/AppearanceConfigSection.vue'

defineProps<{
  modelValue: boolean
}>()

defineEmits<{
  'update:modelValue': [value: boolean]
}>()

// Selected tab. Kept while the app runs, so reopening the dialog returns to it.
const tab = ref('connection')
</script>
