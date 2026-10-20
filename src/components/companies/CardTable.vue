<template>
    <div v-if="cards.length === 0" class="text-grey text-italic">No cards.</div>
    <q-markup-table v-else flat dense separator="horizontal">
        <thead>
            <tr>
                <th class="text-left">Card number</th>
                <th class="text-left">Name</th>
                <th class="text-left">Company on card</th>
                <th class="text-left">Expires</th>
                <th class="text-left">Move to</th>
            </tr>
        </thead>
        <tbody>
            <tr v-for="card in cards" :key="card.card_number">
                <td>{{ card.card_number }}</td>
                <td>{{ card.name || '—' }}</td>
                <td>{{ card.company_name || '—' }}</td>
                <td :class="{ 'text-negative': isExpired(card.expire) }">
                    {{ card.expire ? formatExpire(card.expire) : '—' }}
                </td>
                <td style="min-width: 180px">
                    <q-select
                        dense
                        borderless
                        emit-value
                        map-options
                        :options="options"
                        :model-value="card.company ?? null"
                        @update:model-value="(value) => $emit('move', card.card_number, value)"
                    />
                </td>
            </tr>
        </tbody>
    </q-markup-table>
</template>

<script setup lang="ts">
import type { CardEntry } from 'src/components/models'
import { formatExpire, isExpired } from 'src/components/cardFormatters'

defineProps<{
    cards: CardEntry[]
    options: { label: string; value: string | null }[]
}>()

defineEmits<{
    move: [cardNumber: string, company: string | null]
}>()
</script>
