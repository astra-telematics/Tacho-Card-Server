<template>
    <q-page padding>
        <div class="row items-center q-mb-md">
            <div class="text-h5">Devices</div>
            <q-space />
            <q-btn flat round icon="mdi-refresh" @click="load">
                <q-tooltip>Refresh</q-tooltip>
            </q-btn>
        </div>

        <q-banner v-if="soleCompanyName" rounded class="bg-primary text-white q-mb-md">
            <template #avatar><q-icon name="mdi-information-outline" /></template>
            There's only one company, so every device belongs to <b>{{ soleCompanyName }}</b>
            without being registered. Registering devices matters once there's more than one
            company: a device that isn't registered is then turned away.
        </q-banner>

        <q-card flat bordered class="q-mb-md">
            <q-card-section class="q-pb-none">
                <div class="text-h6">Waiting for a company</div>
                <div class="text-caption text-grey">
                    Devices that tried to connect but aren't registered to any company. They retry
                    on their own, so once assigned they'll connect normally.
                </div>
            </q-card-section>
            <q-card-section>
                <div v-if="view.unknown.length === 0" class="text-grey text-italic">
                    No unknown devices.
                </div>
                <q-markup-table v-else flat dense separator="horizontal">
                    <thead>
                        <tr>
                            <th class="text-left">IMEI</th>
                            <th class="text-left">Registration</th>
                            <th class="text-left">Firmware</th>
                            <th class="text-left">First seen</th>
                            <th class="text-left">Last seen</th>
                            <th class="text-right">Attempts</th>
                            <th class="text-left">Assign to</th>
                        </tr>
                    </thead>
                    <tbody>
                        <tr v-for="device in view.unknown" :key="device.imei">
                            <td>{{ device.imei }}</td>
                            <td :class="{ 'text-grey': !device.vrn }">{{ device.vrn || '—' }}</td>
                            <td>{{ device.firmware_version || '—' }}</td>
                            <td>{{ formatTime(device.first_seen) }}</td>
                            <td>{{ formatTime(device.last_seen) }}</td>
                            <td class="text-right">{{ device.attempts }}</td>
                            <td style="min-width: 180px">
                                <q-select
                                    dense
                                    borderless
                                    emit-value
                                    map-options
                                    placeholder="Choose a company"
                                    :options="companyOptions"
                                    :model-value="null"
                                    @update:model-value="(value) => assign(device.imei, value)"
                                />
                            </td>
                        </tr>
                    </tbody>
                </q-markup-table>
            </q-card-section>
        </q-card>

        <q-card flat bordered>
            <q-card-section class="q-pb-none">
                <div class="text-h6">Registered devices</div>
            </q-card-section>
            <q-card-section class="row q-col-gutter-sm items-start">
                <q-input
                    v-model="newImei"
                    class="col-12 col-sm-5"
                    label="IMEI"
                    dense
                    maxlength="15"
                    :rules="[
                        (v: string) =>
                            v === '' || isImei(v) || '15 digits with a valid check digit',
                    ]"
                    @keyup.enter="addDevice"
                />
                <q-select
                    v-model="newCompany"
                    class="col-12 col-sm-4"
                    label="Company"
                    dense
                    emit-value
                    map-options
                    :options="companyOptions"
                />
                <div class="col-12 col-sm-3">
                    <q-btn
                        unelevated
                        label="Add device"
                        icon="mdi-plus"
                        :color="canAdd ? 'positive' : 'grey'"
                        :disable="!canAdd"
                        @click="addDevice"
                    />
                </div>
            </q-card-section>
            <q-card-section>
                <q-input
                    v-model="filter"
                    dense
                    clearable
                    placeholder="Filter by IMEI, registration or company"
                    class="q-mb-sm"
                >
                    <template #prepend><q-icon name="mdi-magnify" /></template>
                </q-input>
                <div v-if="filteredRegistered.length === 0" class="text-grey text-italic">
                    No registered devices.
                </div>
                <q-markup-table v-else flat dense separator="horizontal">
                    <thead>
                        <tr>
                            <th class="text-left">IMEI</th>
                            <th class="text-left">
                                Registration
                                <q-tooltip>From the device's latest download</q-tooltip>
                            </th>
                            <th class="text-left">Last auth</th>
                            <th class="text-left">
                                Last download
                                <q-tooltip>The latest download that passed its CRC check</q-tooltip>
                            </th>
                            <th class="text-left">Company</th>
                            <th class="text-right"></th>
                        </tr>
                    </thead>
                    <tbody>
                        <tr v-for="device in filteredRegistered" :key="device.imei">
                            <td>{{ device.imei }}</td>
                            <td :class="{ 'text-grey': !device.vrn }">{{ device.vrn || '—' }}</td>
                            <td>
                                <template v-if="device.last_auth_at">
                                    <q-icon
                                        :name="device.last_auth_succeeded ? 'mdi-check-circle' : 'mdi-close-circle'"
                                        :color="device.last_auth_succeeded ? 'positive' : 'negative'"
                                        class="q-mr-xs"
                                    >
                                        <q-tooltip>
                                            {{ device.last_auth_succeeded ? 'Succeeded' : 'Failed' }}
                                        </q-tooltip>
                                    </q-icon>
                                    {{ formatTime(device.last_auth_at) }}
                                </template>
                                <span v-else class="text-grey">—</span>
                            </td>
                            <td v-if="downloading(device.imei)" class="text-primary">
                                <q-spinner-dots size="1.2em" class="q-mr-xs" />
                                Downloading… {{ formatBytes(downloading(device.imei)!.bytes) }}
                                <q-tooltip>
                                    Started {{ formatTime(downloading(device.imei)!.started_at) }}
                                </q-tooltip>
                            </td>
                            <td v-else :class="{ 'text-grey': !device.last_download_at }">
                                {{ device.last_download_at ? formatTime(device.last_download_at) : '—' }}
                            </td>
                            <td style="min-width: 180px">
                                <q-select
                                    dense
                                    borderless
                                    emit-value
                                    map-options
                                    :options="companyOptions"
                                    :model-value="device.company"
                                    @update:model-value="(value) => assign(device.imei, value)"
                                />
                            </td>
                            <td class="text-right">
                                <q-btn
                                    flat
                                    round
                                    dense
                                    icon="mdi-close"
                                    color="negative"
                                    @click="unregister(device.imei)"
                                >
                                    <q-tooltip>Unregister</q-tooltip>
                                </q-btn>
                            </td>
                        </tr>
                    </tbody>
                </q-markup-table>
            </q-card-section>
        </q-card>
    </q-page>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, ref, watch } from 'vue'
import { activeDownloads, formatBytes } from 'src/activeDownloads'
import { Notify } from 'quasar'
import { invoke } from 'src/backend'
import type { CompaniesView, DevicesView } from 'src/components/models'

defineOptions({ name: 'DevicesPage' })

const view = ref<DevicesView>({ sole_company: null, registered: [], unknown: [] })
const companies = ref<CompaniesView['companies']>([])

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
        const [devices, companiesView] = await Promise.all([
            invoke<DevicesView>('get_devices'),
            invoke<CompaniesView>('get_companies'),
        ])
        view.value = devices
        companies.value = companiesView.companies
    } catch (error) {
        notifyError('Failed to load devices', error)
    }
}

// Reload whenever the page is shown: it's kept alive between visits.
onMounted(load)
onActivated(load)

// The device's download in progress, if any.
const downloading = (imei: string) => activeDownloads.value.find((d) => d.imei === imei)
// When one finishes, reload for its new "Last download" time.
watch(
    () => activeDownloads.value.length,
    (count, previous) => {
        if (count < previous) void load()
    },
)

const companyName = (id: string) => companies.value.find((c) => c.id === id)?.name ?? id
const soleCompanyName = computed(() =>
    view.value.sole_company ? companyName(view.value.sole_company) : null,
)
const companyOptions = computed(() => companies.value.map((c) => ({ label: c.name, value: c.id })))

const formatTime = (unixSeconds: number) => new Date(unixSeconds * 1000).toLocaleString()

// Same check as the backend: 15 digits, Luhn check digit.
const isImei = (value: string) => {
    if (!/^\d{15}$/.test(value)) return false
    let sum = 0
    for (let i = 0; i < 15; i++) {
        let digit = Number(value[14 - i])
        if (i % 2 === 1) {
            digit *= 2
            if (digit > 9) digit -= 9
        }
        sum += digit
    }
    return sum % 10 === 0
}

const assign = async (imei: string, company: string | null) => {
    try {
        await invoke('assign_device', { imei, company })
        await load()
    } catch (error) {
        notifyError(`Failed to assign ${imei}`, error)
    }
}

const unregister = (imei: string) => assign(imei, null)

// ---- add ----

const newImei = ref('')
const newCompany = ref<string | null>(null)
const canAdd = computed(() => isImei(newImei.value.trim()) && newCompany.value !== null)

const addDevice = async () => {
    if (!canAdd.value) return
    await assign(newImei.value.trim(), newCompany.value)
    newImei.value = ''
}

// ---- filter ----

const filter = ref<string | null>('')
const filteredRegistered = computed(() => {
    const needle = (filter.value ?? '').trim().toLowerCase()
    if (!needle) return view.value.registered
    return view.value.registered.filter(
        (d) =>
            d.imei.includes(needle) ||
            (d.vrn ?? '').toLowerCase().includes(needle) ||
            companyName(d.company).toLowerCase().includes(needle),
    )
})
</script>
