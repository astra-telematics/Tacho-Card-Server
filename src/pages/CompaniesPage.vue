<template>
    <q-page padding>
        <div class="row items-center q-mb-md">
            <div class="text-h5">Companies</div>
            <q-space />
            <q-btn flat round icon="mdi-refresh" @click="load">
                <q-tooltip>Refresh</q-tooltip>
            </q-btn>
            <q-btn
                unelevated
                color="primary"
                icon="mdi-plus"
                label="Add company"
                @click="openCreate"
            />
        </div>

        <q-banner v-if="view.companies.length === 1" rounded class="bg-primary text-white q-mb-md">
            <template #avatar><q-icon name="mdi-information-outline" /></template>
            There's only one company, so every device and card belongs to it. When you add a second
            company, devices that have already downloaded are registered to
            <b>{{ view.companies[0]?.name }}</b> automatically; any others are assigned on the
            Devices page.
        </q-banner>

        <q-card v-for="company in view.companies" :key="company.id" flat bordered class="q-mb-md">
            <q-card-section class="row items-center q-pb-none">
                <div>
                    <div class="text-h6">{{ company.name }}</div>
                    <div class="text-caption text-grey">
                        id: {{ company.id }} · connection limit {{ company.connection_limit }} ·
                        download interval {{ intervalText(company.download_interval_days) }} ·
                        {{ company.devices.length }} registered device(s)
                    </div>
                </div>
                <q-space />
                <q-btn flat round icon="mdi-pencil" @click="openEdit(company)">
                    <q-tooltip>Edit</q-tooltip>
                </q-btn>
                <q-btn
                    flat
                    round
                    icon="mdi-delete"
                    color="negative"
                    :disable="view.companies.length <= 1"
                    @click="confirmDelete(company)"
                >
                    <q-tooltip>
                        {{
                            view.companies.length <= 1
                                ? "The last company can't be deleted"
                                : 'Delete'
                        }}
                    </q-tooltip>
                </q-btn>
            </q-card-section>
            <q-card-section>
                <CardTable :cards="company.cards" :options="moveOptions" @move="moveCard" />
            </q-card-section>
        </q-card>

        <q-card v-if="view.unassigned_cards.length > 0" flat bordered class="q-mb-md">
            <q-card-section class="q-pb-none">
                <div class="text-h6">Unassigned cards</div>
                <div class="text-caption text-grey">
                    No device can use these until they're assigned to a company.
                </div>
            </q-card-section>
            <q-card-section>
                <CardTable :cards="view.unassigned_cards" :options="moveOptions" @move="moveCard" />
            </q-card-section>
        </q-card>

        <q-dialog v-model="editorOpen">
            <q-card style="min-width: 400px">
                <q-card-section>
                    <div class="text-h6">{{ editor.create ? 'Add company' : 'Edit company' }}</div>
                </q-card-section>
                <q-card-section class="q-gutter-sm">
                    <q-input
                        v-model="editor.name"
                        label="Name"
                        dense
                        autofocus
                        :rules="[(v: string) => v.trim() !== '' || 'Enter a name']"
                        @update:model-value="suggestId"
                    />
                    <q-input
                        v-model="editor.id"
                        label="Id"
                        dense
                        :disable="!editor.create"
                        @update:model-value="editor.idTouched = true"
                        hint="Lowercase letters, digits and '-'. Names its download folder; can't be changed later."
                        :rules="[
                            (v: string) =>
                                isValidId(v) || 'Lowercase letters, digits and \'-\' only',
                        ]"
                    />
                    <q-input
                        v-model.number="editor.connectionLimit"
                        label="Connection limit"
                        type="number"
                        dense
                        :min="1"
                        :max="65535"
                        :rules="[inRange(1, 65535)]"
                        hint="Most of this company's devices connected at once."
                    />
                    <q-input
                        v-model.number="editor.downloadIntervalDays"
                        label="Download interval (days)"
                        type="number"
                        dense
                        :min="0"
                        :max="MAX_DOWNLOAD_INTERVAL_DAYS"
                        :rules="[inRange(0, MAX_DOWNLOAD_INTERVAL_DAYS)]"
                        hint="Minimum days between downloads per device (max 60). 0 = no limit."
                    />
                </q-card-section>
                <q-card-actions align="right">
                    <q-btn flat label="Cancel" v-close-popup />
                    <q-btn
                        unelevated
                        label="Save"
                        :color="editorValid ? 'positive' : 'grey'"
                        :disable="!editorValid"
                        @click="saveEditor"
                    />
                </q-card-actions>
            </q-card>
        </q-dialog>
    </q-page>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, reactive, ref } from 'vue'
import { Notify, useQuasar } from 'quasar'
import { invoke } from 'src/backend'
import CardTable from 'src/components/companies/CardTable.vue'
import type { CompaniesView, Company } from 'src/components/models'
import { inRange, isIntInRange } from 'src/utils'

defineOptions({ name: 'CompaniesPage' })

// Matches MAX_DOWNLOAD_INTERVAL_DAYS in config.rs.
const MAX_DOWNLOAD_INTERVAL_DAYS = 60

const $q = useQuasar()
const view = ref<CompaniesView>({ companies: [], unassigned_cards: [] })

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
        view.value = await invoke<CompaniesView>('get_companies')
    } catch (error) {
        notifyError('Failed to load companies', error)
    }
}

// Reload whenever the page is shown: it's kept alive between visits.
onMounted(load)
onActivated(load)

const intervalText = (days: number) => (days === 0 ? 'none' : days === 1 ? '1 day' : `${days} days`)

// "Move to" choices for a card: every company, or unassigned.
const moveOptions = computed(() => [
    ...view.value.companies.map((c) => ({ label: c.name, value: c.id as string | null })),
    { label: 'Unassigned', value: null },
])

const moveCard = async (cardNumber: string, company: string | null) => {
    try {
        await invoke('assign_card', { cardNumber, company })
        await load()
    } catch (error) {
        notifyError(`Failed to move card ${cardNumber}`, error)
    }
}

// ---- add / edit dialog ----

const isValidId = (id: string) => /^[a-z0-9][a-z0-9-]*$/.test(id)

const editorOpen = ref(false)
const editor = reactive({
    create: true,
    id: '',
    name: '',
    connectionLimit: 10 as number | string,
    downloadIntervalDays: 7 as number | string,
    idTouched: false,
})

const editorValid = computed(
    () =>
        editor.name.trim() !== '' &&
        isValidId(editor.id) &&
        isIntInRange(editor.connectionLimit, 1, 65535) &&
        isIntInRange(editor.downloadIntervalDays, 0, MAX_DOWNLOAD_INTERVAL_DAYS),
)

const openCreate = () => {
    Object.assign(editor, {
        create: true,
        id: '',
        name: '',
        connectionLimit: 10,
        downloadIntervalDays: 7,
        idTouched: false,
    })
    editorOpen.value = true
}

const openEdit = (company: Company) => {
    Object.assign(editor, {
        create: false,
        id: company.id,
        name: company.name,
        connectionLimit: company.connection_limit,
        downloadIntervalDays: company.download_interval_days,
    })
    editorOpen.value = true
}

// While adding, derive the id from the name ("Acme Logistics" → "acme-logistics"),
// until the id has been edited by hand.
const suggestId = (name: string | number | null) => {
    if (!editor.create || editor.idTouched) return
    editor.id = String(name ?? '')
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-+|-+$/g, '')
}

const saveEditor = async () => {
    try {
        const result = await invoke<{ auto_registered_devices: number }>('save_company', {
            id: editor.id,
            name: editor.name,
            connectionLimit: Number(editor.connectionLimit),
            downloadIntervalDays: Number(editor.downloadIntervalDays),
            create: editor.create,
        })
        editorOpen.value = false
        const registered =
            result.auto_registered_devices > 0
                ? ` ${result.auto_registered_devices} existing device(s) were registered to the original company.`
                : ''
        Notify.create({
            message: `Company ${editor.create ? 'added' : 'updated'}.${registered}`,
            color: 'green',
            position: 'bottom',
            timeout: registered ? 8000 : 3000,
        })
        await load()
    } catch (error) {
        notifyError('Failed to save company', error)
    }
}

// ---- delete ----

const confirmDelete = (company: Company) => {
    $q.dialog({
        title: `Delete ${company.name}?`,
        message:
            `Its ${company.cards.length} card(s) become unassigned and its ` +
            `${company.devices.length} registered device(s) become unknown. Downloads ` +
            'already saved are kept.',
        cancel: true,
        ok: { label: 'Delete', color: 'negative', unelevated: true },
    }).onOk(() => {
        void (async () => {
            try {
                await invoke('delete_company', { id: company.id })
                await load()
            } catch (error) {
                notifyError('Failed to delete company', error)
            }
        })()
    })
}
</script>
