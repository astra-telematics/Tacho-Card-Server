<template>
    <q-page padding>
        <div class="row items-center q-mb-md">
            <div class="text-h5">Users</div>
            <q-space />
            <q-btn flat round icon="mdi-refresh" @click="load">
                <q-tooltip>Refresh</q-tooltip>
            </q-btn>
            <q-btn unelevated color="primary" icon="mdi-plus" label="Add user" @click="openAdd" />
        </div>

        <q-banner rounded class="bg-primary text-white q-mb-md">
            <template #avatar><q-icon name="mdi-information-outline" /></template>
            Users log in to the web UI when the server runs headless. Every user can change
            everything, so only give accounts to people who administer this server. The
            <b>admin</b> password is set in config.yaml (<code>admin_password</code>); blank it
            and restart to generate a new one.
        </q-banner>

        <q-card flat bordered>
            <q-card-section>
                <q-markup-table flat dense separator="horizontal">
                    <thead>
                        <tr>
                            <th class="text-left">Username</th>
                            <th class="text-left">Created</th>
                            <th class="text-left">Password set</th>
                            <th class="text-right"></th>
                        </tr>
                    </thead>
                    <tbody>
                        <tr v-for="user in users" :key="user.username">
                            <td>
                                {{ user.username }}
                                <q-badge
                                    v-if="user.username === ADMIN"
                                    outline
                                    color="primary"
                                    label="built-in"
                                    class="q-ml-sm"
                                />
                                <q-badge
                                    v-if="user.username === currentUser"
                                    outline
                                    color="positive"
                                    label="you"
                                    class="q-ml-sm"
                                />
                            </td>
                            <td>{{ formatTime(user.created_at) }}</td>
                            <td>{{ formatTime(user.updated_at) }}</td>
                            <td class="text-right">
                                <template v-if="user.username !== ADMIN">
                                    <q-btn
                                        flat
                                        round
                                        dense
                                        icon="mdi-key-variant"
                                        @click="openPassword(user.username)"
                                    >
                                        <q-tooltip>Change password</q-tooltip>
                                    </q-btn>
                                    <q-btn
                                        flat
                                        round
                                        dense
                                        icon="mdi-delete"
                                        color="negative"
                                        @click="confirmDelete(user.username)"
                                    >
                                        <q-tooltip>Delete</q-tooltip>
                                    </q-btn>
                                </template>
                            </td>
                        </tr>
                    </tbody>
                </q-markup-table>
            </q-card-section>
        </q-card>

        <q-dialog v-model="editorOpen">
            <q-card style="min-width: 360px">
                <q-form @submit="saveEditor">
                    <q-card-section>
                        <div class="text-h6">
                            {{ editor.create ? 'Add user' : `Change password for ${editor.username}` }}
                        </div>
                    </q-card-section>
                    <q-card-section class="q-gutter-sm">
                        <q-input
                            v-if="editor.create"
                            v-model="editor.username"
                            label="Username"
                            dense
                            autofocus
                            autocomplete="off"
                            maxlength="32"
                            :rules="[(v: string) => isValidUsername(v) || USERNAME_HINT]"
                        />
                        <q-input
                            v-model="editor.password"
                            label="Password"
                            type="password"
                            dense
                            :autofocus="!editor.create"
                            autocomplete="new-password"
                            :rules="[(v: string) => v.length >= MIN_PASSWORD || PASSWORD_HINT]"
                        />
                        <q-input
                            v-model="editor.confirm"
                            label="Confirm password"
                            type="password"
                            dense
                            autocomplete="new-password"
                            :rules="[(v: string) => v === editor.password || 'Passwords differ']"
                        />
                    </q-card-section>
                    <q-card-actions align="right">
                        <q-btn flat label="Cancel" v-close-popup />
                        <q-btn
                            unelevated
                            type="submit"
                            label="Save"
                            :color="editorValid ? 'positive' : 'grey'"
                            :disable="!editorValid"
                        />
                    </q-card-actions>
                </q-form>
            </q-card>
        </q-dialog>
    </q-page>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, reactive, ref } from 'vue'
import { Notify, useQuasar } from 'quasar'
import { currentUser, invoke } from 'src/backend'
import type { UserAccount } from 'src/components/models'

defineOptions({ name: 'UsersPage' })

// Match users.rs.
const ADMIN = 'admin'
const MIN_PASSWORD = 8
const USERNAME_HINT = "1–32 letters, digits, '.', '_' or '-'"
const PASSWORD_HINT = `At least ${MIN_PASSWORD} characters`

const $q = useQuasar()
const users = ref<UserAccount[]>([])

const notifyError = (message: string, error: unknown) => {
    console.error(message, error)
    Notify.create({
        message: typeof error === 'string' ? `${message}: ${error}` : message,
        color: 'red',
        position: 'bottom',
        timeout: 5000,
    })
}

const load = async () => {
    try {
        users.value = await invoke<UserAccount[]>('get_users')
    } catch (error) {
        notifyError('Failed to load users', error)
    }
}

// Reload whenever the page is shown: it's kept alive between visits.
onMounted(load)
onActivated(load)

const formatTime = (unixSeconds: number) => new Date(unixSeconds * 1000).toLocaleString()

const isValidUsername = (name: string) =>
    /^[A-Za-z0-9._-]{1,32}$/.test(name.trim()) && name.trim().toLowerCase() !== ADMIN

// ---- add / change password dialog ----

const editorOpen = ref(false)
const editor = reactive({ create: true, username: '', password: '', confirm: '' })

const editorValid = computed(
    () =>
        (!editor.create || isValidUsername(editor.username)) &&
        editor.password.length >= MIN_PASSWORD &&
        editor.password === editor.confirm,
)

const openAdd = () => {
    Object.assign(editor, { create: true, username: '', password: '', confirm: '' })
    editorOpen.value = true
}

const openPassword = (username: string) => {
    Object.assign(editor, { create: false, username, password: '', confirm: '' })
    editorOpen.value = true
}

const saveEditor = async () => {
    if (!editorValid.value) return
    try {
        const args = { username: editor.username.trim(), password: editor.password }
        await invoke(editor.create ? 'add_user' : 'set_user_password', args)
        Notify.create({
            message: editor.create ? `User ${args.username} added.` : 'Password changed.',
            color: 'green',
            position: 'bottom',
            timeout: 3000,
        })
        editorOpen.value = false
        await load()
    } catch (error) {
        notifyError(editor.create ? 'Failed to add user' : 'Failed to change password', error)
    }
}

// ---- delete ----

const confirmDelete = (username: string) => {
    $q.dialog({
        title: `Delete ${username}?`,
        message:
            username === currentUser.value
                ? "That's you: you'll be logged out."
                : "They'll be logged out and can't log in again.",
        cancel: true,
        ok: { label: 'Delete', color: 'negative', unelevated: true },
    }).onOk(() => {
        void (async () => {
            try {
                await invoke('delete_user', { username })
                await load()
            } catch (error) {
                notifyError(`Failed to delete ${username}`, error)
            }
        })()
    })
}
</script>
