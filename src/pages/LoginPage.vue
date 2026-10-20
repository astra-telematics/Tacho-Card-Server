<template>
    <q-layout view="lHh Lpr lFf">
        <q-page-container>
            <q-page class="flex flex-center">
                <q-card flat bordered style="width: 360px; max-width: 90vw">
                    <q-card-section>
                        <div class="text-h6">Tacho Card Server</div>
                        <div class="text-caption text-grey">Log in to manage this server.</div>
                    </q-card-section>
                    <q-form @submit="submit">
                        <q-card-section class="q-gutter-sm">
                            <q-input
                                v-model="username"
                                label="Username"
                                dense
                                autofocus
                                autocomplete="username"
                            />
                            <q-input
                                v-model="password"
                                label="Password"
                                type="password"
                                dense
                                autocomplete="current-password"
                            />
                            <div v-if="errorMessage" class="text-negative text-caption">
                                {{ errorMessage }}
                            </div>
                        </q-card-section>
                        <q-card-actions align="right">
                            <q-btn
                                unelevated
                                type="submit"
                                label="Log in"
                                :color="canSubmit ? 'positive' : 'grey'"
                                :disable="!canSubmit"
                                :loading="busy"
                            />
                        </q-card-actions>
                    </q-form>
                </q-card>
            </q-page>
        </q-page-container>
    </q-layout>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { Dark } from 'quasar'
import { useRoute, useRouter } from 'vue-router'
import { login } from 'src/backend'

defineOptions({ name: 'LoginPage' })

const route = useRoute()
const router = useRouter()

const username = ref('')
const password = ref('')
const busy = ref(false)
const errorMessage = ref('')

const canSubmit = computed(() => username.value.trim() !== '' && password.value !== '' && !busy.value)

// The saved theme needs a login to read; follow the system until then.
onMounted(() => Dark.set('auto'))

const submit = async () => {
    if (!canSubmit.value) return
    busy.value = true
    errorMessage.value = ''
    try {
        await login(username.value.trim(), password.value)
        const redirect = typeof route.query.redirect === 'string' ? route.query.redirect : '/'
        // Only go back into this app, never to another site.
        await router.replace(redirect.startsWith('/') && !redirect.startsWith('//') ? redirect : '/')
    } catch (error) {
        errorMessage.value = typeof error === 'string' ? error : 'Login failed.'
        password.value = ''
    } finally {
        busy.value = false
    }
}
</script>
