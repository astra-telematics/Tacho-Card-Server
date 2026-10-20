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
                    <q-btn v-if="!isDesktop" flat round icon="mdi-account-circle">
                        <q-menu>
                            <q-list style="min-width: 160px">
                                <q-item-label header>Logged in as {{ currentUser }}</q-item-label>
                                <q-item v-close-popup clickable @click="logout">
                                    <q-item-section avatar><q-icon name="mdi-logout" /></q-item-section>
                                    <q-item-section>Log out</q-item-section>
                                </q-item>
                            </q-list>
                        </q-menu>
                    </q-btn>
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
import { currentUser, isDesktop, listen, logout, type UnlistenFn } from 'src/backend'
import 'animate.css'
import ServerConfigDialog from 'src/components/ServerConfigDialog.vue'
import { applyTheme, getConfig, type AppConfig } from 'src/utils'

defineOptions({
    name: 'MainLayout',
})

const configOpen = ref(false)
const serverStatus = ref(-1)

// Registered Tauri listeners. Stored so we can detach them on unmount —
// otherwise stale handlers keep mutating dead reactive state and pile up
// across hot reloads in dev.
const unlistenFns: UnlistenFn[] = []

const ACCESS_NOTIFICATION_TEXT =
    "The application cannot access the directory '~/Documents/tcs' and cannot continue to operate. Perhaps such a directory has already been created by another version of the program, therefore it has local access restrictions. A possible solution may be: rename the current directory, for example, to tcs1 and restart the application. It will create a new directory with the necessary access rights."

onMounted(async () => {
    // Apply the saved theme at startup (and after a webview reload). Lives here rather
    // than in the config dialog, whose contents only exist while it's open. The event is
    // triggered by TachoMainComponent's `frontend-loaded`, which can race this listener
    // being registered, so also fetch once afterwards: whichever arrives first applies.
    // Empty means the backend hasn't loaded its config yet; the event will follow.
    try {
        const unlisten = await listen('global-config-appearance', (event) => {
            applyTheme((event.payload as AppConfig).dark_theme)
        })
        unlistenFns.push(unlisten)
        const { dark_theme } = await getConfig()
        if (dark_theme) applyTheme(dark_theme)
    } catch (error) {
        console.error('Error loading theme:', error)
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
