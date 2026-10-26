import { formatDate, formatDateTime } from 'src/utils'

// Display formatters for CardConfig fields populated by the APDU sniffer.
// Used by SmartCardList.vue and TachoMainComponent.vue.

import type { SmartCard } from './models'

/// Builds the meta string shown in parentheses after the card number,
/// e.g. "Gen1 | Company Card". Omits missing parts.
export function formatCardMeta(
  card: Pick<SmartCard, 'structure_version' | 'card_type'>,
): string {
  const parts: string[] = []
  if (card.structure_version) parts.push(formatStructureVersion(card.structure_version))
  if (card.card_type != null) parts.push(formatCardType(card.card_type))
  return parts.join(' | ')
}

export function formatCardType(type: number | null | undefined): string {
  switch (type) {
    case 1:
      return 'Driver Card'
    case 2:
      return 'Workshop Card'
    case 3:
      return 'Control Card'
    case 4:
      return 'Company Card'
    default:
      return `Unknown (${type})`
  }
}

export function formatStructureVersion(v: [number, number] | null | undefined): string {
  if (!v) return ''
  const [major, minor] = v
  if (major === 0 && minor === 0) return 'Gen1'
  if (major === 1 && minor === 0) return 'Gen2 v1'
  if (major === 1 && minor === 1) return 'Gen2 v2'
  return `v${major}.${minor}`
}

export function formatExpire(unixTs: number | null | undefined): string {
  if (!unixTs) return ''
  return formatDate(unixTs, true)
}

export function isExpired(unixTs: number | null | undefined): boolean {
  if (!unixTs) return false
  return unixTs * 1000 < Date.now()
}

/// Formats last_auth timestamp (stored as UTC unix seconds) in the viewer's time zone
/// and date format. Storage stays UTC — only display is localised.
export function formatAuthDate(unixTs: number | null | undefined): string {
  if (!unixTs) return ''
  return formatDateTime(unixTs)
}
