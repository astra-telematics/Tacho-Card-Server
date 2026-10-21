<template>
    <q-layout view="lHh Lpr lFf">
        <q-header elevated>
            <q-toolbar>
                <q-toolbar-title class="q-ml-md">
                    Tacho Card Server
                    <q-icon
                        name="mdi-record-circle-outline"
                        class="q-ml-md"
                        :color="serverStatus >= 0 ? 'green' : undefined"
                    />
                    <span class="q-ml-xs">{{ serverStatus }}</span>
                    <!-- Downloads in progress: details on hover, per device on the Devices page. -->
                    <q-chip
                        v-if="activeDownloads.length > 0"
                        dense
                        clickable
                        color="white"
                        text-color="primary"
                        class="q-ml-md"
                        @click="router.push('/devices')"
                    >
                        <q-spinner-dots size="1em" class="q-mr-xs" />
                        {{ activeDownloads.length }} downloading
                        <q-tooltip>
                            <div v-for="download in activeDownloads" :key="download.imei">
                                {{ download.imei }}: {{ formatBytes(download.bytes) }}
                            </div>
                        </q-tooltip>
                    </q-chip>
                </q-toolbar-title>

                <q-tabs inline-label no-caps shrink>
                    <q-route-tab
                        to="/"
                        exact
                        icon="mdi-card-account-details-outline"
                        label="Readers"
                    />
                    <q-route-tab to="/companies" icon="mdi-domain" label="Companies" />
                    <q-route-tab to="/devices" icon="mdi-truck-outline" label="Devices" />
                    <q-route-tab to="/users" icon="mdi-account-multiple-outline" label="Users" />
                </q-tabs>

                <div class="q-pa-xs q-gutter-sm">
                    <q-btn flat round icon="mdi-cog" @click="configOpen = true" />
                    <ServerConfigDialog v-model="configOpen" />
                    <!-- Account menu. The desktop app has no login, so it only has the theme. -->
                    <q-btn
                        flat
                        round
                        :icon="isDesktop ? 'mdi-theme-light-dark' : 'mdi-account-circle'"
                    >
                        <q-menu>
                            <q-list style="min-width: 200px">
                                <q-item-label v-if="!isDesktop" header>
                                    Logged in as {{ currentUser }}
                                </q-item-label>
                                <q-item>
                                    <q-item-section>
                                        <div class="text-caption text-grey q-mb-xs">Theme</div>
                                        <q-btn-toggle
                                            :model-value="theme"
                                            no-caps
                                            rounded
                                            unelevated
                                            size="sm"
                                            toggle-color="primary"
                                            :options="THEME_OPTIONS"
                                            @update:model-value="changeTheme"
                                        />
                                    </q-item-section>
                                </q-item>
                                <template v-if="!isDesktop">
                                    <q-separator />
                                    <q-item v-close-popup clickable @click="passwordOpen = true">
                                        <q-item-section avatar>
                                            <q-icon name="mdi-key-variant" />
                                        </q-item-section>
                                        <q-item-section>Change password</q-item-section>
                                    </q-item>
                                    <q-item v-close-popup clickable @click="logout">
                                        <q-item-section avatar>
                                            <q-icon name="mdi-logout" />
                                        </q-item-section>
                                        <q-item-section>Log out</q-item-section>
                                    </q-item>
                                </template>
                            </q-list>
                        </q-menu>
                    </q-btn>
                    <ChangePasswordDialog v-if="!isDesktop" v-model="passwordOpen" />
                </div>
            </q-toolbar>
        </q-header>

        <q-page-container>
            <!-- keep-alive: the Readers page holds live reader/card state from backend
                 events, so it mustn't be rebuilt when switching pages. -->
            <router-view v-slot="{ Component }">
                <keep-alive>
                    <component :is="Component" />
                </keep-alive>
            </router-view>
        </q-page-container>
    </q-layout>
</template>

<script setup lang="ts">
import { Notify } from 'quasar'
import { ref, onMounted, onUnmounted } from 'vue'
import { currentUser, invoke, isDesktop, listen, logout, type UnlistenFn } from 'src/backend'
import 'animate.css'
import { useRouter } from 'vue-router'
import { activeDownloads, formatBytes, trackActiveDownloads } from 'src/activeDownloads'
import ChangePasswordDialog from 'src/components/ChangePasswordDialog.vue'
import ServerConfigDialog from 'src/components/ServerConfigDialog.vue'
import { applyTheme, THEME_OPTIONS } from 'src/utils'

defineOptions({
    name: 'MainLayout',
})

const router = useRouter()
const configOpen = ref(false)
const passwordOpen = ref(false)
const serverStatus = ref(-1)

// The logged-in user's theme (admin's on the desktop). Applied straight away, then saved.
const theme = ref('Auto')
const changeTheme = async (value: string) => {
    const previous = theme.value
    theme.value = value
    applyTheme(value)
    try {
        await invoke('set_my_theme', { theme: value })
    } catch (error) {
        theme.value = previous
        applyTheme(previous)
        Notify.create({
            message: typeof error === 'string' ? `Failed to save the theme: ${error}` : 'Failed to save the theme.',
            color: 'red',
            position: 'bottom',
            timeout: 4000,
        })
    }
}

// Registered Tauri listeners. Stored so we can detach them on unmount —
// otherwise stale handlers keep mutating dead reactive state and pile up
// across hot reloads in dev.
const unlistenFns: UnlistenFn[] = []

const ACCESS_NOTIFICATION_TEXT =
    "The application cannot access the directory '~/Documents/tcs' and cannot continue to operate. Perhaps such a directory has already been created by another version of the program, therefore it has local access restrictions. A possible solution may be: rename the current directory, for example, to tcs1 and restart the application. It will create a new directory with the necessary access rights."

onMounted(async () => {
    // Apply the user's theme. The layout is rebuilt after each login, so a different
    // user gets theirs.
    try {
        theme.value = await invoke<string>('get_my_theme')
        applyTheme(theme.value)
    } catch (error) {
        console.error('Error loading theme:', error)
    }

    try {
        unlistenFns.push(await trackActiveDownloads())
    } catch (error) {
        console.error('Error following active downloads:', error)
    }

    try {
        const unlisten = await listen('server-status', (event) => {
            serverStatus.value = event.payload as number
            // console.log('server-status:', serverStatus.value)
        })
        unlistenFns.push(unlisten)
    } catch (error) {
        console.error('Error listening to server-status:', error)
    }

    try {
        const unlisten = await listen('global-notification', (event) => {
            const raw = event.payload
            if (!raw || typeof raw !== 'object') return
            const payload = raw as { notification_type?: unknown; message?: unknown }
            const type =
                typeof payload.notification_type === 'string' ? payload.notification_type : ''
            const message = typeof payload.message === 'string' ? payload.message : ''

            console.log('global-notification:', type, 'message:', message)

            if (type === 'access') {
                Notify.create({
                    message: ACCESS_NOTIFICATION_TEXT,
                    color: 'red',
                    position: 'bottom',
                    timeout: 999000,
                })
            } else if (type === 'version') {
                // Use the backend-provided message verbatim — it contains the new
                // version string and the download URL.
                Notify.create({
                    message: message || 'A new version is available.',
                    color: 'green',
                    position: 'bottom',
                    timeout: 15000,
                    classes: 'animate__animated animate__shakeX',
                })
            } else {
                console.log('global-notification: unknown type:', type)
            }
        })
        unlistenFns.push(unlisten)
    } catch (error) {
        console.error('Error listening to global-notification:', error)
    }
})

onUnmounted(() => {
    while (unlistenFns.length > 0) {
        const fn = unlistenFns.pop()
        try {
            fn?.()
        } catch (e) {
            console.error('Error detaching Tauri listener:', e)
        }
    }
})
</script>
