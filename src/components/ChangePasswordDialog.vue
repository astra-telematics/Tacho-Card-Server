<template>
    <!-- The logged-in user changes their own password (web UI only). -->
    <q-dialog :model-value="modelValue" @update:model-value="$emit('update:modelValue', $event)">
        <q-card style="min-width: 360px">
            <q-form @submit="save">
                <q-card-section>
                    <div class="text-h6">Change your password</div>
                    <div v-if="currentUser === ADMIN" class="text-caption text-grey">
                        config.yaml will then hold only a hash of it. If it's ever lost, blank
                        <code>admin_password</code> there and restart to get a new one.
                    </div>
                </q-card-section>
                <q-card-section class="q-gutter-sm">
                    <q-input
                        v-model="form.current"
                        label="Current password"
                        type="password"
                        dense
                        autofocus
                        autocomplete="current-password"
                    />
                    <q-input
                        v-model="form.password"
                        label="New password"
                        type="password"
                        dense
                        autocomplete="new-password"
                        :rules="[(v: string) => v.length >= MIN_PASSWORD || PASSWORD_HINT]"
                    />
                    <q-input
                        v-model="form.confirm"
                        label="Confirm new password"
                        type="password"
                        dense
                        autocomplete="new-password"
                        :rules="[(v: string) => v === form.password || 'Passwords differ']"
                    />
                </q-card-section>
                <q-card-actions align="right">
                    <q-btn flat label="Cancel" v-close-popup />
                    <q-btn
                        unelevated
                        type="submit"
                        label="Change"
                        :color="valid ? 'positive' : 'grey'"
                        :disable="!valid || saving"
                        :loading="saving"
                    />
                </q-card-actions>
            </q-form>
        </q-card>
    </q-dialog>
</template>

<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue'
import { Notify } from 'quasar'
import { currentUser, invoke } from 'src/backend'

const props = defineProps<{ modelValue: boolean }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()

const ADMIN = 'admin'
const MIN_PASSWORD = 8 // users.rs MIN_PASSWORD_LENGTH
const PASSWORD_HINT = `At least ${MIN_PASSWORD} characters`

const form = reactive({ current: '', password: '', confirm: '' })
const saving = ref(false)

// Start empty each time it opens.
watch(
    () => props.modelValue,
    (open) => {
        if (open) Object.assign(form, { current: '', password: '', confirm: '' })
    },
)

const valid = computed(
    () =>
        form.current !== '' &&
        form.password.length >= MIN_PASSWORD &&
        form.password === form.confirm,
)

const save = async () => {
    if (!valid.value) return
    saving.value = true
    try {
        await invoke('change_my_password', {
            currentPassword: form.current,
            newPassword: form.password,
        })
        Notify.create({
            message: 'Password changed. Your other sessions have been logged out.',
            color: 'green',
            position: 'bottom',
            timeout: 4000,
        })
        emit('update:modelValue', false)
    } catch (error) {
        Notify.create({
            message: typeof error === 'string' ? error : 'Failed to change the password.',
            color: 'red',
            position: 'bottom',
            timeout: 5000,
        })
    } finally {
        saving.value = false
    }
}
</script>
