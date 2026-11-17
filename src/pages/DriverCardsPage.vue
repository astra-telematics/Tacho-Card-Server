<template>
    <q-page padding>
        <div class="row items-center q-mb-md">
            <div class="text-h5">Driver cards</div>
            <q-space />
            <q-btn flat round icon="mdi-refresh" @click="load">
                <q-tooltip>Refresh</q-tooltip>
            </q-btn>
        </div>

        <q-card flat bordered>
            <q-card-section class="q-pb-none">
                <div class="text-caption text-grey">
                    Every driver card downloaded through a vehicle so far, with its latest download.
                    A card is due again after its company's card download interval, and must be
                    downloaded at least every {{ LEGAL_INTERVAL_DAYS }} days. A device announces a
                    card as soon as it's inserted, so a due card is downloaded on its next trip.
                </div>
            </q-card-section>
            <q-card-section>
                <div class="row q-col-gutter-sm q-mb-sm">
                    <q-input
                        v-model="filter"
                        class="col-12 col-sm-8"
                        dense
                        clearable
                        placeholder="Filter by card number, registration, IMEI or company"
                    >
                        <template #prepend><q-icon name="mdi-magnify" /></template>
                    </q-input>
                    <q-select
                        v-model="statusFilter"
                        class="col-12 col-sm-4"
                        dense
                        emit-value
                        map-options
                        :options="statusOptions"
                    />
                </div>
                <div v-if="filtered.length === 0" class="text-grey text-italic">
                    {{ view.cards.length === 0 ? 'No driver cards downloaded yet.' : 'No matching cards.' }}
                </div>
                <q-markup-table v-else flat dense separator="horizontal">
                    <thead>
                        <tr>
                            <th class="text-left">Card number</th>
                            <th class="text-left">Company</th>
                            <th class="text-left">
                                Last download
                                <q-tooltip>The latest download of the card that passed its CRC check</q-tooltip>
                            </th>
                            <th class="text-left">Vehicle</th>
                            <th class="text-left">Next due</th>
                            <th class="text-left">Status</th>
                            <th class="text-right">
                                Downloads
                                <q-tooltip>Verified downloads that included the card</q-tooltip>
                            </th>
                        </tr>
                    </thead>
                    <tbody>
                        <tr v-for="card in filtered" :key="`${card.company}/${card.card_number}`">
                            <td style="font-family: monospace">{{ card.card_number }}</td>
                            <td>{{ companyName(card.company) }}</td>
                            <td>
                                {{ formatDateTime(card.last_download_at) }}
                                <span class="text-grey">({{ daysAgoText(card.last_download_date) }})</span>
                            </td>
                            <td>
                                <span :class="{ 'text-grey': !card.vrn }">{{ card.vrn || '—' }}</span>
                                <span class="text-grey"> · {{ card.slot === 2 ? 'co-driver' : 'driver' }} slot</span>
                                <q-tooltip>Device {{ card.imei }}</q-tooltip>
                            </td>
                            <td :class="{ 'text-grey': !card.next_due_date }">
                                {{ card.next_due_date ? formatLocalDate(card.next_due_date) : 'No limit' }}
                            </td>
                            <td>
                                <q-badge :color="status(card).color" :label="status(card).label" />
                                <q-tooltip>
                                    Must be downloaded by {{ formatLocalDate(card.legal_due_date) }}
                                </q-tooltip>
                            </td>
                            <td class="text-right">{{ card.downloads }}</td>
                        </tr>
                    </tbody>
                </q-markup-table>
            </q-card-section>
        </q-card>
    </q-page>
</template>

<script setup lang="ts">
import { computed, onActivated, onMounted, ref, watch } from 'vue'
import { Notify } from 'quasar'
import { invoke } from 'src/backend'
import { activeDownloads } from 'src/activeDownloads'
import { formatDateTime } from 'src/utils'
import type { CompaniesView, DriverCard, DriverCardsView } from 'src/components/models'

defineOptions({ name: 'DriverCardsPage' })

// Matches DRIVER_CARD_LEGAL_INTERVAL_DAYS in config.rs.
const LEGAL_INTERVAL_DAYS = 28

const view = ref<DriverCardsView>({ today: '', cards: [] })
const companies = ref<CompaniesView['companies']>([])

const load = async () => {
    try {
        const [cards, companiesView] = await Promise.all([
            invoke<DriverCardsView>('get_driver_cards'),
            invoke<CompaniesView>('get_companies'),
        ])
        view.value = cards
        companies.value = companiesView.companies
    } catch (error) {
        console.error('Failed to load driver cards', error)
        Notify.create({
            message: typeof error === 'string' ? `Failed to load driver cards: ${error}` : 'Failed to load driver cards',
            color: 'red',
            position: 'bottom',
            timeout: 5000,
        })
    }
}

// Reload whenever the page is shown (it's kept alive between visits), and when a
// download finishes, since it may have included a card.
onMounted(load)
onActivated(load)
watch(
    () => activeDownloads.value.length,
    (count, previous) => {
        if (count < previous) void load()
    },
)

const companyName = (id: string) => companies.value.find((c) => c.id === id)?.name ?? id

// The dates are the server's local calendar dates (YYYY-MM-DD), which the intervals
// count in - shown as dates, without converting through a time zone.
const parseLocalDate = (date: string) => {
    const [y, m, d] = date.split('-').map(Number)
    return new Date(y ?? 1970, (m ?? 1) - 1, d ?? 1)
}
const formatLocalDate = (date: string) => parseLocalDate(date).toLocaleDateString()
const daysBetween = (from: string, to: string) =>
    Math.round((parseLocalDate(to).getTime() - parseLocalDate(from).getTime()) / 86_400_000)
const daysAgoText = (date: string) => {
    const days = daysBetween(date, view.value.today)
    return days <= 0 ? 'today' : days === 1 ? 'yesterday' : `${days} days ago`
}

type Status = 'overdue' | 'due' | 'ok'
const statusOf = (card: DriverCard): Status => {
    const today = view.value.today
    if (today >= card.legal_due_date) return 'overdue'
    if (card.next_due_date && today >= card.next_due_date) return 'due'
    return 'ok'
}
const STATUS = {
    overdue: { label: 'Overdue', color: 'negative' },
    due: { label: 'Due', color: 'warning' },
    ok: { label: 'Up to date', color: 'positive' },
} as const
const status = (card: DriverCard) => STATUS[statusOf(card)]

// ---- filters ----

const filter = ref<string | null>('')
const statusFilter = ref<'all' | 'attention' | Status>('all')
const statusOptions = [
    { label: 'All cards', value: 'all' },
    { label: 'Due or overdue', value: 'attention' },
    { label: 'Overdue', value: 'overdue' },
    { label: 'Up to date', value: 'ok' },
]

const filtered = computed(() => {
    const needle = (filter.value ?? '').trim().toLowerCase()
    return view.value.cards
        .filter((card) => {
            const s = statusOf(card)
            if (statusFilter.value === 'attention') return s !== 'ok'
            return statusFilter.value === 'all' || s === statusFilter.value
        })
        .filter(
            (card) =>
                !needle ||
                card.card_number.toLowerCase().includes(needle) ||
                (card.vrn ?? '').toLowerCase().includes(needle) ||
                card.imei.includes(needle) ||
                companyName(card.company).toLowerCase().includes(needle),
        )
        // Most urgent first: overdue, then due, then by when they were last downloaded.
        .sort((a, b) => {
            const rank = { overdue: 0, due: 1, ok: 2 }
            return rank[statusOf(a)] - rank[statusOf(b)] || a.last_download_at - b.last_download_at
        })
})
</script>
